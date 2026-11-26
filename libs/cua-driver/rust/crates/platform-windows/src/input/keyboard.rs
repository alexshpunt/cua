// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Background keyboard injection via PostMessage.
//!
//! WM_CHAR — posts a character code; simpler and more reliable for text entry.
//! WM_KEYDOWN/WM_KEYUP — used for non-printable keys (Enter, Tab, arrows, F-keys, etc.)
//!
//! All messages are posted async (PostMessageW), so they do NOT steal focus.
//!
//! Modern XAML / WinUI3 / UWP targets reject PostMessage-based keyboard
//! injection because their CoreInput dispatcher only consumes events from
//! the system input queue, not posted messages. This module exposes
//! [`is_xaml_host_hwnd`] so callers (e.g. the `type_text` tool) can route
//! around the PostMessage path for those targets — see CUA-543 and
//! `tools::type_text::TypeTextTool` for the routing logic. The actual UIA
//! `ValuePattern.SetValue` injection lives in the tools layer alongside
//! the existing `set_value` tool; this module deliberately stays
//! Win32-only so the unit tests don't depend on UIA initialisation.

use anyhow::{bail, Result};
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetGUIThreadInfo, GetParent, GetWindowTextW,
    GetWindowThreadProcessId, IsChild, PostMessageW, GUITHREADINFO, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

use crate::typing_turn::{restores_previous, ForegroundInputError, TypingTurn};
use crate::win32::{foreground_matches_target_or_owned_window, windows::ForegroundTarget};
use cua_driver_core::type_pacing;

// ── XAML / UWP host detection ────────────────────────────────────────────────
//
// Two routing signals, OR'd:
//   1. Top-level window class name matches a known XAML host class.
//   2. Owning process .exe basename matches a known XAML-hosted .exe.
//
// The EXE-basename signal is the more reliable of the two: cross-session
// `GetClassNameW` can return nothing, and modern apps like Win 11 Notepad
// keep the legacy `"Notepad"` window class even though they render XAML
// underneath. Diagnostic data captured by `tools::DebugWindowInfoTool`
// (see CUA-543) confirms `notepad.exe` is the reliable signal for modern
// Notepad; class name is not.

const XAML_HOST_CLASSES: &[&str] = &[
    "ApplicationFrameWindow",
    "WinUIDesktopWin32WindowClass",
    "Windows.UI.Core.CoreWindow",
    "Microsoft.UI.Content.DesktopChildSiteBridge",
];

const XAML_HOST_EXES: &[&str] = &[
    "notepad.exe",              // Win 11 modern Notepad (UWP-packaged)
    "calculatorapp.exe",        // UWP Calculator
    "calc.exe",                 // some Win 11 builds expose the stub directly
    "applicationframehost.exe", // generic UWP frame host
    "photos.exe",               // UWP Photos
    "systemsettings.exe",       // modern Settings
];

fn class_name(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }
}

fn owning_exe_basename(hwnd: HWND) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut pid: u32 = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if tid == 0 || pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len: u32 = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    if result.is_err() || len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&path)
        .to_ascii_lowercase();
    Some(name)
}

/// Names a window as `<title> (<process>)` so a refusal says which foreign
/// window, such as a system modal, holds the foreground.
fn window_label(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return "no foreground window".to_string();
    }
    let mut title = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut title) };
    let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
    let exe = owning_exe_basename(hwnd).unwrap_or_else(|| "unknown process".to_string());
    format!("{title} ({exe})")
}

fn foreign_foreground_error(actual: HWND, detail: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "foreign window holds the foreground: {label}; {detail} Nothing was retried; the \
         foreground owner must be closed or dismissed before the rest can be sent.",
        label = window_label(actual),
    )
}

/// `true` iff the given HWND should bypass the PostMessage keyboard
/// path and route through UIA patterns (or another non-PostMessage
/// mechanism). See module docs + CUA-543 for the routing rationale.
pub fn is_xaml_host_hwnd(hwnd: u64) -> bool {
    let h = HWND(hwnd as *mut _);
    if let Some(cls) = class_name(h) {
        if XAML_HOST_CLASSES.iter().any(|known| cls == *known) {
            return true;
        }
    }
    if let Some(exe) = owning_exe_basename(h) {
        if XAML_HOST_EXES.iter().any(|known| exe == *known) {
            return true;
        }
    }
    false
}

const KEY_DELAY_MS: u64 = 4;

/// If any UI thread under the target has a focused child window that's a
/// descendant of `parent`, return that child. Otherwise `None`. Used to retarget
/// `PostMessage(WM_CHAR/WM_KEYDOWN)` from the top-level frame to the actual
/// editor control (Scintilla in Notepad++, RichEdit in WordPad, etc.) —
/// top-level WindowProcs don't forward keyboard messages to embedded editors
/// automatically, so without this drill-down `type_text` silently no-ops
/// against any app that puts its text surface in a child HWND.
///
/// Embedded renderers such as WebView2 may put their focused child on a
/// different UI thread from the native top-level frame. Enumerating descendant
/// thread ids is therefore required; checking only the frame thread queues the
/// message successfully but leaves the renderer untouched. More than one of
/// those threads can retain a focused HWND, so choose the deepest focused
/// descendant rather than whichever thread happens to enumerate first.
fn focused_descendant(parent: HWND) -> Option<HWND> {
    if parent.0.is_null() {
        return None;
    }
    let parent_thread = unsafe { GetWindowThreadProcessId(parent, None) };
    if parent_thread == 0 {
        return None;
    }

    unsafe extern "system" fn collect_thread(child: HWND, lparam: LPARAM) -> BOOL {
        let threads = &mut *(lparam.0 as *mut Vec<u32>);
        let thread = GetWindowThreadProcessId(child, None);
        if thread != 0 && !threads.contains(&thread) {
            threads.push(thread);
        }
        TRUE
    }

    let mut target_threads = vec![parent_thread];
    unsafe {
        let _ = EnumChildWindows(
            Some(parent),
            Some(collect_thread),
            LPARAM(&mut target_threads as *mut Vec<u32> as isize),
        );
    }
    let mut best: Option<(usize, HWND)> = None;
    for target_thread in target_threads {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(target_thread, &mut info) }.is_err() {
            continue;
        }
        let focused = info.hwndFocus;
        if focused.0.is_null()
            || focused == parent
            || !unsafe { IsChild(parent, focused) }.as_bool()
        {
            continue;
        }

        let mut depth = 0usize;
        let mut current = focused;
        while current != parent && depth < 64 {
            let Ok(next) = (unsafe { GetParent(current) }) else {
                break;
            };
            if next.0.is_null() {
                break;
            }
            depth += 1;
            current = next;
        }
        if current == parent && best.as_ref().is_none_or(|(d, _)| depth > *d) {
            best = Some((depth, focused));
        }
    }
    best.map(|(_, focused)| focused)
}

/// XAML islands take posted keys only on their input-site child window: neither the
/// top-level frame nor the island bridge forwards them, and `GUITHREADINFO.hwndFocus`
/// does not name the input site while the app is in the background (LPT-1846).
fn is_input_site_class(class: &str) -> bool {
    matches!(
        class,
        "InputSiteWindowClass" | "Windows.UI.Input.InputSite.WindowClass"
    )
}

/// The window that hosts a XAML island inside a Win32 frame.
fn is_island_bridge_class(class: &str) -> bool {
    class.ends_with("DesktopChildSiteBridge") || class.ends_with("DesktopWindowContentBridge")
}

/// The first descendant of `parent` whose class matches.
fn descendant_of_class(parent: HWND, matches: fn(&str) -> bool) -> Option<HWND> {
    struct Search {
        matches: fn(&str) -> bool,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if class_name(child).is_some_and(|class| (search.matches)(&class)) {
            search.found = Some(child);
            return BOOL(0);
        }
        TRUE
    }
    let mut search = Search {
        matches,
        found: None,
    };
    unsafe {
        let _ = EnumChildWindows(
            parent,
            Some(visit),
            LPARAM(&mut search as *mut Search as isize),
        );
    }
    search.found
}

/// The window that posted keys and text for `root` must reach:
/// - a direct Chromium/Electron frame takes them on the top-level window itself;
/// - otherwise the focused descendant of the target's UI threads (an editor child such
///   as RichEdit or Scintilla, a rename box), unless that is a XAML island bridge;
/// - a XAML island takes them on its input site, a UWP frame on its CoreWindow.
pub(crate) fn keyboard_target(root: HWND) -> HWND {
    if crate::input::is_chromium_target_window(root.0 as u64) {
        return root;
    }
    let focused = focused_descendant(root);
    if let Some(focused) = focused {
        let class = class_name(focused).unwrap_or_default();
        if is_input_site_class(&class) {
            return focused;
        }
        if !is_island_bridge_class(&class) {
            return focused;
        }
        if let Some(site) = descendant_of_class(focused, is_input_site_class) {
            return site;
        }
    }
    if let Some(focused) = focused {
        return focused;
    }
    // No thread of the target has a focus window, which is the case after the app was in
    // the foreground and lost it: Windows clears its thread focus. Keys then go to the
    // window that holds the app's content, the largest visible child at each level: a
    // document editor such as Notepad's RichEdit, or the input site of a XAML island that
    // covers the window (Windows Terminal, Paint). Hidden and small islands such as a
    // menu bar are skipped.
    content_window(root)
        .or_else(|| descendant_of_class(root, |class| class == "Windows.UI.Core.CoreWindow"))
        .unwrap_or(root)
}

/// The largest visible child at each level below `root`, ending at a window with no
/// visible children, or at the input site of a XAML island bridge.
fn content_window(root: HWND) -> Option<HWND> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};
    struct Children {
        parent: HWND,
        best: Option<(i64, HWND)>,
    }
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Children);
        // Direct children only; EnumChildWindows walks the whole subtree.
        if GetParent(child).ok() != Some(search.parent) || !IsWindowVisible(child).as_bool() {
            return TRUE;
        }
        if class_name(child).is_some_and(|class| class == "InputNonClientPointerSource") {
            return TRUE;
        }
        let mut rect = RECT::default();
        if GetWindowRect(child, &mut rect).is_err() {
            return TRUE;
        }
        let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
        if area > 0 && search.best.is_none_or(|(best, _)| area > best) {
            search.best = Some((area, child));
        }
        TRUE
    }
    let mut current = root;
    let mut chosen = None;
    for _ in 0..16 {
        if class_name(current).is_some_and(|class| is_island_bridge_class(&class)) {
            return descendant_of_class(current, is_input_site_class).or(Some(current));
        }
        let mut search = Children {
            parent: current,
            best: None,
        };
        unsafe {
            let _ = EnumChildWindows(
                current,
                Some(visit),
                LPARAM(&mut search as *mut Children as isize),
            );
        }
        let Some((_, child)) = search.best else {
            break;
        };
        current = child;
        chosen = Some(child);
    }
    chosen
}

/// Chromium stops reading posted keys once it has been activated and deactivated
/// again (its web contents is blurred). A posted activation and focus message makes it
/// read them again without taking the foreground (LPT-1846).
fn wake_chromium_keyboard(root: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{WM_ACTIVATE, WM_SETFOCUS};
    if !crate::input::is_chromium_target_window(root.0 as u64) {
        return;
    }
    const WA_ACTIVE: usize = 1;
    unsafe {
        let _ = PostMessageW(root, WM_ACTIVATE, WPARAM(WA_ACTIVE), LPARAM(0));
        let _ = PostMessageW(root, WM_SETFOCUS, WPARAM(0), LPARAM(0));
    }
}

/// Set when posted input left the target's UI thread in a Win32 menu loop.
pub(crate) const BACKGROUND_MENU_LOOP: &str = "background_menu_loop";

/// A posted Alt chord or F10 can start a Win32 menu loop. In a background app that
/// loop blocks the app (its own message loop stops) until the menu ends, so end it
/// at once and report that the key needs the foreground.
fn cancel_background_menu_loop(target: HWND) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GUI_INMENUMODE, GUI_POPUPMENUMODE, WM_CANCELMODE,
    };
    let thread = unsafe { GetWindowThreadProcessId(target, None) };
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if thread == 0 || unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() {
        return Ok(());
    }
    if info.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) == 0 {
        return Ok(());
    }
    let owner = if info.hwndMenuOwner.0.is_null() {
        target
    } else {
        info.hwndMenuOwner
    };
    unsafe {
        let _ = PostMessageW(owner, WM_CANCELMODE, WPARAM(0), LPARAM(0));
    }
    bail!(
        "{BACKGROUND_MENU_LOOP}: the key opened a Win32 menu, which stops the app while it is in \
         the background; the menu was closed again"
    )
}

/// Wait for an element-focused embedded renderer to expose its child HWND.
/// UIA SetFocus can complete before WebView2 updates GUITHREADINFO; polling the
/// observable focus target avoids posting the key to the native frame in that
/// short interval.
pub fn wait_for_focused_descendant(hwnd: u64, timeout: Duration) -> Option<u64> {
    let parent = HWND(hwnd as *mut _);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(target) = focused_descendant(parent) {
            return Some(target.0 as usize as u64);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(10));
    }
}

/// Post a Unicode character as WM_CHAR.
pub fn post_char(hwnd: u64, ch: char) -> Result<()> {
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(hwnd) {
        anyhow::bail!(msg);
    }
    // Retarget to the focused child if any — top-level WindowProcs typically
    // don't forward WM_CHAR to embedded editor children (Scintilla, RichEdit).
    let h_parent = HWND(hwnd as *mut _);
    wake_chromium_keyboard(h_parent);
    let h = keyboard_target(h_parent);
    let code = ch as u32 as usize;
    unsafe {
        PostMessageW(Some(h), WM_CHAR, WPARAM(code), LPARAM(1))?;
    }
    Ok(())
}

/// Post `\n` (or `\r`, or `\r\n`) as a real Enter keystroke pair —
/// WM_KEYDOWN/WM_KEYUP(VK_RETURN) — to the focused child `h`.
///
/// Why a keystroke and not WM_CHAR(0x0A) or WM_CHAR(0x0D): standard Win32
/// edit controls accept `WM_CHAR(0x0D)` (carriage return) and insert a
/// newline; richer controls (LibreOffice's VCL edit, modern WPF/WinForms,
/// Scintilla) ignore `0x0A` outright and only sometimes accept `0x0D`.
/// A real `WM_KEYDOWN(VK_RETURN)` is the universally-honoured "Enter
/// pressed" signal — it produces a paragraph break in word processors,
/// activates the default button in dialogs, and submits forms in browsers.
/// The previous `WM_CHAR(0x0A)`-as-newline path silently joined every
/// line of multi-line `type_text` input into a single run (visible in the
/// LibreOffice Writer ode-to-a-background-cursor screenshot).
unsafe fn post_enter_keystroke(h: HWND) -> Result<()> {
    post_vk_keystroke(h, windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN)
}

/// Post `vk` as a WM_KEYDOWN/WM_KEYUP pair to `h`, with the hold gap
/// [`post_enter_keystroke`] explains. Used for the keys `type_text` keeps
/// real: Return and Tab.
unsafe fn post_vk_keystroke(h: HWND, vk: VIRTUAL_KEY) -> Result<()> {
    let scan = MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC);
    let lp_down = 1u32 | (scan << 16);
    let lp_up = lp_down | (1u32 << 30) | (1u32 << 31);
    PostMessageW(
        Some(h),
        WM_KEYDOWN,
        WPARAM(vk.0 as usize),
        LPARAM(lp_down as isize),
    )?;
    // Hold time between KEYDOWN and KEYUP — matches `post_key`'s pattern and
    // gives the target's message loop time to TranslateMessage the KEYDOWN
    // (which synthesizes WM_CHAR(0x0D)) and DispatchMessage the paragraph
    // break before the KEYUP arrives. Without this gap, LibreOffice Writer
    // intermittently produced a double paragraph break AND dropped the
    // next character — visible in the "ABC\nDEF\nGHI" repro as "ABC / DEF /
    // (gap) / HI".
    sleep(Duration::from_millis(KEY_DELAY_MS));
    PostMessageW(
        Some(h),
        WM_KEYUP,
        WPARAM(vk.0 as usize),
        LPARAM(lp_up as isize),
    )?;
    Ok(())
}

/// Post all characters in a string as WM_CHAR messages with inter-key delay.
///
/// Line breaks (`\n`, `\r`, or `\r\n`) are emitted as Enter keystrokes
/// (see [`post_enter_keystroke`]) instead of literal `WM_CHAR(0x0A/0x0D)`,
/// which most rich-text Win32 controls drop.
pub fn post_type_text(hwnd: u64, text: &str) -> Result<()> {
    post_type_text_with_delay(hwnd, text, 0)
}

/// Post all characters in `text` as WM_CHAR messages with a configurable
/// inter-character delay (on top of the baseline KEY_DELAY_MS gap).
///
/// Line breaks (`\n`, `\r`, or `\r\n`) are emitted as Enter keystrokes
/// (see [`post_enter_keystroke`]) — see the LibreOffice screenshot in
/// the PR description for the bug this fixes.
pub fn post_type_text_with_delay(hwnd: u64, text: &str, inter_char_ms: u64) -> Result<()> {
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(hwnd) {
        anyhow::bail!(msg);
    }
    let h_parent = HWND(hwnd as *mut _);
    // Resolve the receiver once at entry — re-querying per-character would
    // race with text-insertion side effects on the focus.
    wake_chromium_keyboard(h_parent);
    let h = keyboard_target(h_parent);

    let units = text_units(text);
    let batched = type_pacing::is_batched(inter_char_ms);
    // Batched: up to BATCH_UNITS posts, then one pump barrier, so the target
    // never holds more than a batch and the call returns with a settled field.
    // Paced: one unit per range, each followed by its pause.
    let ranges = if batched {
        type_pacing::plan_chunks(&unit_kinds(&units), type_pacing::BATCH_UNITS)
    } else {
        (0..units.len()).map(|i| i..i + 1).collect()
    };
    let mut since_barrier = 0usize;
    for range in ranges {
        let batch_len = range.len();
        for unit in &units[range] {
            match unit {
                // The Enter keystroke makes the target's TranslateMessage append a
                // WM_CHAR(0x0D) to ITS queue when it dispatches the WM_KEYDOWN.
                // Anything we post before that lands ahead of the Return and the
                // text is reordered. Wait for the target to pump (twice, because a
                // sent message can overtake the posted keydown) before moving on.
                // Tab is the same: a real key press that the target translates.
                TextUnit::Return | TextUnit::Tab => {
                    unsafe {
                        match unit {
                            TextUnit::Tab => post_vk_keystroke(
                                h,
                                windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB,
                            )?,
                            _ => post_enter_keystroke(h)?,
                        }
                    }
                    wait_for_target_pump(h);
                    // Extra settle after Enter: paragraph creation in rich
                    // editors (VCL Writer, Scintilla, RichEdit) is heavier than
                    // a single-character insert.
                    sleep(Duration::from_millis(if batched {
                        KEY_DELAY_MS
                    } else {
                        KEY_DELAY_MS + inter_char_ms + 20
                    }));
                    since_barrier = 0;
                }
                // One UTF-16 unit per WM_CHAR. A supplementary scalar (emoji) is a
                // high then a low surrogate; posting the scalar value in a single
                // WM_CHAR is truncated by the target and shows up as garbage.
                TextUnit::Packet(code) => {
                    unsafe {
                        PostMessageW(Some(h), WM_CHAR, WPARAM(*code as usize), LPARAM(1))?;
                    }
                    since_barrier += 1;
                    if !batched {
                        sleep(Duration::from_millis(KEY_DELAY_MS + inter_char_ms));
                        // Bound the target's backlog so the read-back after the
                        // call sees a settled field, and so a hung target aborts
                        // the call.
                        if since_barrier >= POST_CHUNK_UNITS {
                            wait_for_target_pump(h);
                            since_barrier = 0;
                        }
                    }
                }
                // Only foreground delivery maps characters to layout keys (`keyed`).
                TextUnit::Key { .. } => {
                    unreachable!(
                        "posted text is built by text_units, which never makes layout keys"
                    )
                }
            }
        }
        if batched && since_barrier > 0 {
            wait_for_target_pump(h);
            // A pump round trip does not prove the target has dispatched the
            // posted characters: sent messages are serviced ahead of posted
            // ones. Give it time to drain the batch, so the backlog stays
            // bounded to one batch; the caller's read-back repairs the rest.
            sleep(Duration::from_millis(
                BATCH_DRAIN_MS_PER_UNIT * batch_len as u64,
            ));
            since_barrier = 0;
        }
    }
    wait_for_target_pump(h);
    Ok(())
}

/// Drain time per posted unit at the end of a batched `WM_CHAR` batch.
const BATCH_DRAIN_MS_PER_UNIT: u64 = 2;

/// Posted characters between two pump barriers.
const POST_CHUNK_UNITS: usize = 32;

/// Round-trip `WM_NULL` to `h`'s thread twice. Sent messages are serviced
/// whenever the target's message loop next retrieves a message, so this
/// returns once the target has gone around its loop after everything posted
/// so far. Bounded and best effort: a hung target just times out.
fn wait_for_target_pump(h: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL,
    };
    for _ in 0..2 {
        let mut out = 0usize;
        let _ = unsafe {
            SendMessageTimeoutW(
                h,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                250,
                Some(&mut out),
            )
        };
    }
}

/// Press a named key (and optional modifiers) via WM_KEYDOWN/WM_KEYUP.
///
/// Modifiers are held in the target thread's key state for the press (see
/// [`crate::input::key_state`]); no modifier key messages are posted. Posted
/// modifier keydowns do not change what `GetKeyState` reports, so the app saw a bare
/// key (Ctrl+A typed a control character), and a posted Alt press and release can
/// open a menu bar by itself.
pub fn post_key(hwnd: u64, key: &str, modifiers: &[&str]) -> Result<()> {
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(hwnd) {
        anyhow::bail!(msg);
    }
    let hwnd_win = HWND(hwnd as *mut _);
    wake_chromium_keyboard(hwnd_win);
    // WebView2/Tauri keeps the editable renderer in a focused child HWND, a XAML
    // island reads keys on its input site: posting only to the top-level frame
    // reports success but never reaches them.
    let target = keyboard_target(hwnd_win);
    let (vk, scan) = match resolve_press(target, key, modifiers)? {
        Press::Key { vk, scan } => (vk, scan),
        Press::Text(ch) => return post_type_text(hwnd, &ch.to_string()),
    };
    let has_alt = modifiers
        .iter()
        .any(|m| matches!(m.to_ascii_lowercase().as_str(), "alt" | "menu" | "option"));

    let repeat_lp = |scan: u32, extended: bool, key_up: bool| {
        let mut lp: u32 = 1; // repeat count
        lp |= scan << 16;
        if extended {
            lp |= 1 << 24;
        }
        if has_alt {
            // Context code: Alt is down.
            lp |= 1 << 29;
        }
        if key_up {
            lp |= (1 << 30) | (1 << 31);
        }
        LPARAM(lp as isize)
    };

    let (down_msg, up_msg) = if has_alt {
        (WM_SYSKEYDOWN, WM_SYSKEYUP)
    } else {
        (WM_KEYDOWN, WM_KEYUP)
    };

    let held: Vec<u16> = modifiers
        .iter()
        .flat_map(|m| crate::input::key_state::modifier_keys(m).iter().copied())
        .collect();
    let mut held = crate::input::key_state::HeldKeys::hold(target, &held)?;
    unsafe {
        PostMessageW(
            Some(target),
            down_msg,
            WPARAM(vk.0 as usize),
            repeat_lp(scan, is_extended(vk), false),
        )?;
        sleep(Duration::from_millis(KEY_DELAY_MS));
        PostMessageW(
            Some(target),
            up_msg,
            WPARAM(vk.0 as usize),
            repeat_lp(scan, is_extended(vk), true),
        )?;
    }
    if let Some(keys) = held.as_mut() {
        keys.posted(2);
    }
    // Dropping the held keys waits until the target processed the press.
    match held {
        Some(keys) => drop(keys),
        None => wait_for_target_pump(target),
    }
    cancel_background_menu_loop(target)
}

/// Press `key` (with optional `modifiers`) via `SendInput` against the system
/// input queue, briefly focusing `hwnd` so the keystrokes land there.
///
/// Why this exists alongside `post_key`: `PostMessage(WM_KEYDOWN, VK_CONTROL)`
/// puts a message in the target's queue but does NOT update the system-wide
/// modifier state that apps poll via `GetKeyState` / `GetAsyncKeyState`. For
/// any Win32 app whose accelerator dispatcher uses `TranslateAccelerator` (which
/// is most native Win32 apps — LibreOffice, FAR, classic Notepad, etc.), the
/// shortcut never fires; the `s` arrives as plain text input.
///
/// `SendInput` puts the synthesized events on the **system input queue** —
/// the same queue `GetKeyState` reads from — so `Ctrl+S` is properly detected
/// as an accelerator. This requires foreground activation. Returning the
/// previous foreground requires supported evidence covering every inserted
/// event, with no earlier unconfirmed prefix.
///
/// Windows may refuse the foreground swap (`SetForegroundWindow` is
/// restricted when not driven by user input), and UIPI drops input sent to a
/// higher-integrity target. Both are detected and reported as errors; no
/// input is sent to an unconfirmed window.
pub(crate) fn send_key_synthesized(hwnd: u64, key: &str, modifiers: &[&str]) -> Result<TypingTurn> {
    send_key_synthesized_after_focus(hwnd, key, modifiers, TypingTurn::NotSent, || {
        Ok(TypingTurn::NotSent)
    })
}

/// Foreground key delivery with a target-specific focus step performed only
/// after Windows has confirmed the exact top-level HWND as foreground.
///
/// Element-addressed callers use this to avoid the activation/focus race:
/// UIA `SetFocus` before `SetForegroundWindow` can be overwritten by the
/// ensuing activation. The callback establishes and verifies child focus. No
/// keyboard input is inserted when confirmation fails; any focus click is tracked.
pub(crate) fn send_key_synthesized_after_focus(
    hwnd: u64,
    key: &str,
    modifiers: &[&str],
    prior: TypingTurn,
    focus: impl FnOnce() -> Result<TypingTurn>,
) -> Result<TypingTurn> {
    crate::human_input::check().map_err(anyhow::Error::msg)?;
    let target = HWND(hwnd as *mut _);
    if target.0.is_null() {
        bail!("invalid target hwnd");
    }
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(hwnd) {
        // Same UIPI defense as the PostMessage path. SendInput from UIAccess
        // _is_ allowed cross-integrity, but if our daemon is somehow at a
        // lower integrity than target, SendInput would land in the wrong
        // window (we couldn't set foreground). Better to surface the
        // diagnostic early than silently no-op.
        bail!(msg);
    }
    let (key_vk, key_scan) = match resolve_press(target, key, modifiers)? {
        Press::Key { vk, scan } => (vk, scan),
        Press::Text(ch) => {
            return send_text_synthesized_after_focus(hwnd, &ch.to_string(), prior, focus)
        }
    };
    let mod_vks: Vec<VIRTUAL_KEY> = modifiers.iter().filter_map(|m| modifier_vk(m)).collect();

    // Build the INPUT sequence: modifiers down, key down, key up, modifiers up
    // (reverse order). Each event sends the scancode + EXTENDEDKEY flag where
    // appropriate so apps that read scancodes (not virtual keys) work too.
    let mut events: Vec<INPUT> = Vec::with_capacity(mod_vks.len() * 2 + 2);
    for mvk in &mod_vks {
        events.push(key_input(*mvk, false));
    }
    events.push(scanned_key_input(key_vk, key_scan, false));
    events.push(scanned_key_input(key_vk, key_scan, true));
    for mvk in mod_vks.iter().rev() {
        events.push(key_input(*mvk, true));
    }

    let mut keys = mod_vks;
    keys.push(key_vk);
    let observable = chord_read_split(&events, &keys).is_some();
    with_confirmed_foreground(
        target,
        "key delivery",
        prior,
        observable,
        focus,
        |attachment, ft, turn| {
            let read = match attachment {
                Some(attachment) => attachment.send_and_await_read(
                    &events,
                    Some(&keys),
                    ft,
                    Duration::from_secs(2),
                    false,
                )?,
                None => insert_unobservable(&events),
            };
            *turn = turn.combine(read.turn());
            match read {
                UnitRead::Read | UnitRead::Unobservable => Ok(()),
                UnitRead::Short(sent) => bail!(
                    "SendInput inserted only {sent} of {} events. No input was retried; inspect \
                     the target before repeating the action.",
                    events.len()
                ),
                UnitRead::Stalled => bail!(
                    "input_delivery_stalled: Windows did not confirm reading the requested key \
                     pulses within 2000 ms. They were inserted and may still appear in the target. \
                     Nothing was retried."
                ),
            }
        },
    )
}

/// Foreground-delivery text entry: the `delivery_mode:"foreground"` rung for
/// `type_text`. Symmetric with [`send_key_synthesized`] — briefly fronts the
/// target, types `text` as SendInput Unicode (`KEYEVENTF_UNICODE`, so every
/// codepoint lands regardless of keyboard layout). Foreground return requires
/// supported read evidence covering the complete inserted stream. This is only
/// reached on the explicit `delivery_mode:"foreground"` rung (background never
/// fronts); it does NOT silently fall back to
/// PostMessage: if the foreground swap is rejected
/// or the input is blocked, it bails with the same diagnostic
/// `send_key_synthesized` returns, so the caller gets an honest error instead
/// of a false success. Required for VCL/LibreOffice document grids and other
/// targets where PostMessage WM_CHAR is silently dropped.
/// Receiver-read completion is returned separately from successful insertion.
pub(crate) fn send_text_synthesized(hwnd: u64, text: &str) -> Result<TypingTurn> {
    send_text_synthesized_after_focus(hwnd, text, TypingTurn::NotSent, || Ok(TypingTurn::NotSent))
}

/// Foreground Unicode delivery with child focus established after exact
/// top-level activation and before `SendInput`.
pub(crate) fn send_text_synthesized_after_focus(
    hwnd: u64,
    text: &str,
    prior: TypingTurn,
    focus: impl FnOnce() -> Result<TypingTurn>,
) -> Result<TypingTurn> {
    send_text_synthesized_paced_after_focus(hwnd, text, type_pacing::DEFAULT_DELAY_MS, prior, focus)
}

/// [`send_text_synthesized_after_focus`] with the `delay_ms` contract: 0 sends
/// batches of up to [`type_pacing::BATCH_UNITS`] units per SendInput; a positive value sends
/// one unit at a time and pauses `delay_ms` after each.
pub(crate) fn send_text_synthesized_paced_after_focus(
    hwnd: u64,
    text: &str,
    delay_ms: u64,
    prior: TypingTurn,
    focus: impl FnOnce() -> Result<TypingTurn>,
) -> Result<TypingTurn> {
    crate::human_input::check().map_err(anyhow::Error::msg)?;
    let target = HWND(hwnd as *mut _);
    if target.0.is_null() {
        bail!("invalid target hwnd");
    }
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(hwnd) {
        bail!(msg);
    }
    // Build the key-event sequence. Characters on the target's keyboard layout
    // are real keystrokes (see `keyed`); other codepoints go through as Unicode
    // packets (KEYEVENTF_UNICODE, wVk=0, wScan=code unit), and line breaks are
    // mapped to a real VK_RETURN keystroke — mirroring the background path
    // (`post_enter_keystroke`) — because terminals and rich editors honour an
    // Enter key event, not a raw `\r`/`\n` Unicode packet. `\r\n` collapses to
    // a single Return (the `\r` emits the Enter; the following `\n` is silent).
    // The layout of the window that reads the keys: an embedded foreground
    // window's own thread can use another layout than the target's.
    let reader = embedded_foreground(target).unwrap_or(target);
    let layout = unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout(GetWindowThreadProcessId(
            reader, None,
        ))
    };
    let caps_lock = unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(
            windows::Win32::UI::Input::KeyboardAndMouse::VK_CAPITAL.0 as i32,
        )
    } & 1
        == 1;
    let units = keyed(text_units(text), layout, caps_lock);
    if units.is_empty() {
        return Ok(prior);
    }
    let n = units.len();
    let ranges = if type_pacing::is_batched(delay_ms) {
        type_pacing::plan_chunks(&unit_kinds(&units), type_pacing::BATCH_UNITS)
    } else {
        (0..n).map(|i| i..i + 1).collect()
    };
    // Repeated keys and Unicode packets lack a per-key read witness. Attach
    // to pace their queue anyway, without treating the marker as a receipt.
    let observable = true;

    with_confirmed_foreground(
        target,
        "text delivery",
        prior,
        observable,
        focus,
        |attachment, ft, turn| {
            // Without an attached reader, multiple batches cannot be paced.
            // Refuse before inserting any input rather than flood a busy queue.
            if attachment.is_none() && ranges.len() > 1 {
                bail!(
                    "input_read_unobservable: cannot pace multiple text batches; no text was sent"
                );
            }
            for range in ranges {
                let k = range.start;
                if let Err(stop) = crate::human_input::check() {
                    bail!(
                        "{stop} Stopped after inserting {k} of {n} text units; the remaining {} \
                         were not sent.",
                        n - k
                    );
                }
                let actual = unsafe { GetForegroundWindow() };
                if !foreground_matches_target_or_owned_window(ft, actual.0 as usize as u64) {
                    *turn = TypingTurn::Lost;
                    bail!(
                        "foreground_lost: foreground moved to HWND {h:?} after inserting {k} of \
                         {n} text units; the remaining {remaining} were not sent. Nothing was \
                         retried; inspect the target before sending the rest.",
                        h = actual.0,
                        remaining = n - k,
                    );
                }
                let batch = &units[range.clone()];
                let events = batch_events(batch);
                let keys = batch_pulse_keys(batch);
                let read = match attachment {
                    Some(attachment) => attachment.send_and_await_read(
                        &events,
                        keys.as_deref(),
                        ft,
                        Duration::from_secs(2),
                        true,
                    )?,
                    None => insert_unobservable(&events),
                };
                *turn = turn.combine(read.turn());
                let first = k + 1;
                let last = range.end;
                match read {
                    UnitRead::Read | UnitRead::Unobservable => {}
                    UnitRead::Short(sent) => bail!(
                        "SendInput inserted only {sent} of {total} key events for text units \
                         {first}..{last} of {n}; {k} earlier units were inserted. No input was \
                         retried; inspect the target before sending the rest.",
                        total = events.len()
                    ),
                    UnitRead::Stalled => bail!(
                        "text_delivery_stalled: Windows did not confirm progress for text units \
                         {first}..{last} of {n} within 2000 ms; {k} earlier units were inserted. \
                         Those units may still appear in the target. Nothing was retried; inspect \
                         the target before sending the rest."
                    ),
                }
                if delay_ms > 0 && last < n {
                    sleep(Duration::from_millis(delay_ms));
                }
            }
            Ok(())
        },
    )
}

fn wait_for_exact_foreground(target: HWND, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if unsafe { GetForegroundWindow() } == target {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10));
    }
}

/// Synchronize pre-input activation, not input delivery. Foreground activation
/// crosses input queues asynchronously; joining while its nudge is pending can
/// prevent the receiver from retrieving input until we detach.
pub(crate) fn activation_ready(target: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL};
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            target,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            500,
            Some(&mut result),
        )
        .0 != 0
    }
}
/// The foreground window when it is embedded in `target` (see
/// [`crate::win32::embedded_in_target`]): keys and text for the target go to it.
fn embedded_foreground(target: HWND) -> Option<HWND> {
    let foreground = unsafe { GetForegroundWindow() };
    crate::win32::embedded_in_target(target.0 as usize as u64, foreground.0 as usize as u64)
        .then_some(foreground)
}

/// The active and focus HWNDs of `hwnd`'s GUI thread, as that thread's own
/// input queue records them. Read without attaching.
fn gui_thread_active_and_focus(hwnd: HWND) -> Option<(HWND, HWND)> {
    let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if thread == 0 {
        return None;
    }
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.ok()?;
    Some((info.hwndActive, info.hwndFocus))
}

/// Whether `foreground`'s thread has processed its own activation.
///
/// `GetForegroundWindow` changes the moment `SetForegroundWindow` succeeds,
/// but the target thread only becomes active when it reads the activation
/// event Windows queued for it; until then its queue has no active or focus
/// window. Attaching to the queue in that interval hands the pending
/// activation, and every key routed by it, to the attached driver thread,
/// which never reads input: the target stays inactive and reads nothing
/// until the attachment is dropped (cua#8543). The queue's own active window
/// is the observable proof that activation was processed.
fn thread_activation_processed(foreground: HWND) -> bool {
    gui_thread_active_and_focus(foreground).is_some_and(|(active, _)| active == foreground)
}

pub(crate) fn wait_for_thread_activation(foreground: HWND, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if thread_activation_processed(foreground) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10));
    }
}

fn activation_not_processed_error(
    target: HWND,
    foreground: HWND,
    operation: &str,
) -> anyhow::Error {
    let (active, focus) = gui_thread_active_and_focus(foreground).unwrap_or_default();
    anyhow::anyhow!(
        "foreground_unavailable: HWND {:?} is foreground for {operation} but its thread did \
         not process the activation within 500 ms (its queue's active HWND {:?}, focus HWND \
         {:?}); no input was sent. Target HWND {:?}.",
        foreground.0,
        active.0,
        focus.0,
        target.0
    )
}

/// Run one system-queue input transaction while `target`, or a live visible
/// same-process window whose owner chain reaches `target`, is foreground.
/// A window embedded in `target` that already has the foreground, such as a
/// Unity Editor inside a Herdr tab, takes the input instead: activating
/// `target` would take the keyboard from it.
/// Foreground and optional child focus are checked against
/// external Win32/UIA state instead of inferred from API return values or a
/// fixed settle delay. The body records in `turn` what it inserted; the prior
/// foreground is restored only as [`restores_previous`] allows, and a failed
/// turn that keeps it returns [`ForegroundInputError`].
fn with_confirmed_foreground(
    target: HWND,
    operation: &str,
    prior: TypingTurn,
    observable: bool,
    focus: impl FnOnce() -> Result<TypingTurn>,
    body: impl FnOnce(Option<&InputQueueAttachment>, ForegroundTarget, &mut TypingTurn) -> Result<()>,
) -> Result<TypingTurn> {
    let previous = unsafe { GetForegroundWindow() };
    let embedded = embedded_foreground(target).is_some();
    if !embedded {
        let _ = unsafe { crate::input::force_foreground_assisted(target) };
    }
    // Foreground assistance can insert its own unlock key. That prefix also
    // needs read evidence, including when activation fails before the body.
    let prior = if crate::focus_return::has_unconfirmed_input() {
        prior.combine(TypingTurn::Unobservable)
    } else {
        prior
    };
    if !embedded && !wait_for_exact_foreground(target, Duration::from_millis(500)) {
        let actual = unsafe { GetForegroundWindow() };
        if !previous.0.is_null() && previous != target && restores_previous(prior) {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
        return Err(ForegroundInputError {
            error: anyhow::anyhow!(
                "foreground_unavailable: Windows did not confirm exact target HWND {:?} for {operation} \
                 within 500 ms (actual foreground HWND {:?}); no requested input was sent.",
                target.0,
                actual.0
            ),
            turn: prior,
        }
        .into());
    }
    // Confirm the current recipient's activation without joining its queue.
    // Do not return the foreground after an unconfirmed activation prefix.
    let recipient = unsafe { GetForegroundWindow() };
    if !wait_for_thread_activation(recipient, Duration::from_millis(500)) {
        let error = activation_not_processed_error(target, recipient, operation);
        if !previous.0.is_null() && previous != recipient && restores_previous(prior) {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
        return Err(ForegroundInputError { error, turn: prior }.into());
    }

    let Some(foreground_target) = crate::win32::capture_foreground_target(target.0 as usize as u64)
    else {
        if !previous.0.is_null() && previous != target && restores_previous(prior) {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
        return Err(ForegroundInputError {
            error: anyhow::anyhow!(
                "foreground_unavailable: exact target HWND {:?} disappeared before {operation}; \
                 no requested input was sent",
                target.0
            ),
            turn: prior,
        }
        .into());
    };

    // A focus callback reports whether it inserted global input. Attach only
    // after a no-input focus step; never reset an earlier pointer prefix's state.
    let mut attachment = None;
    let mut turn = prior;
    let result = (|| {
        turn = turn.combine(focus()?);
        let deadline = Instant::now() + Duration::from_millis(250);
        let actual = loop {
            let actual = unsafe { GetForegroundWindow() };
            if foreground_matches_target_or_owned_window(
                foreground_target,
                actual.0 as usize as u64,
            ) || Instant::now() >= deadline
            {
                break actual;
            }
            sleep(Duration::from_millis(10));
        };
        if !foreground_matches_target_or_owned_window(foreground_target, actual.0 as usize as u64) {
            bail!(
                "foreground_unavailable: exact target HWND {:?}, a verified same-process \
                 owned window or embedded window was not foreground while preparing \
                 {operation} (actual foreground HWND {:?}); no keyboard input was sent",
                target.0,
                actual.0
            );
        }
        if !wait_for_thread_activation(actual, Duration::from_millis(500)) {
            return Err(activation_not_processed_error(target, actual, operation));
        }
        if observable && activation_ready(actual) {
            attachment = InputQueueAttachment::attach(actual);
        }
        let reader = attachment
            .as_ref()
            .filter(|attachment| attachment.matches_foreground(actual));
        body(reader, foreground_target, &mut turn)
    })();
    if let Err(error) = &result {
        if let Some(kept) = error.downcast_ref::<ForegroundInputError>() {
            turn = turn.combine(kept.turn);
        }
    }
    if crate::focus_return::has_unconfirmed_input() {
        turn = turn.combine(TypingTurn::Unobservable);
    }
    let actual = unsafe { GetForegroundWindow() };
    if !foreground_matches_target_or_owned_window(foreground_target, actual.0 as usize as u64) {
        turn = turn.combine(
            if crate::win32::foreground_after_target_closed(
                foreground_target,
                actual.0 as usize as u64,
            ) {
                TypingTurn::Unobservable
            } else {
                TypingTurn::Lost
            },
        );
    }
    drop(attachment);
    if !restores_previous(turn) {
        crate::focus_return::note_unconfirmed_input();
        return result
            .map(|()| turn)
            .map_err(|error| ForegroundInputError { error, turn }.into());
    }
    // The outer lease returns the human's remembered window, not necessarily
    // the window that happened to be foreground before this call.
    if !previous.0.is_null()
        && previous != target
        && !crate::focus_return::active()
        && !crate::focus_return::has_unconfirmed_input()
    {
        let _ = unsafe { SetForegroundWindow(previous) };
    }
    result
        .map(|()| turn)
        .map_err(|error| ForegroundInputError { error, turn }.into())
}

#[derive(Clone, Copy)]
enum UnitRead {
    Read,
    Unobservable,
    Short(u32),
    Stalled,
}

impl UnitRead {
    fn turn(self) -> TypingTurn {
        match self {
            Self::Read => TypingTurn::Drained,
            Self::Unobservable => TypingTurn::Unobservable,
            Self::Short(0) => TypingTurn::NotSent,
            Self::Short(_) | Self::Stalled => TypingTurn::Undrained,
        }
    }
}

/// Construct releases for inserted DOWNs without replaying an inserted UP.
fn inserted_key_releases(events: &[INPUT], inserted: usize) -> Vec<INPUT> {
    let mut held: Vec<KEYBDINPUT> = Vec::new();
    for event in events.iter().take(inserted) {
        let key = unsafe { event.Anonymous.ki };
        let same = |held: &KEYBDINPUT| held.wVk == key.wVk && held.wScan == key.wScan;
        if key.dwFlags.0 & KEYEVENTF_KEYUP.0 == 0 {
            held.push(key);
        } else if let Some(index) = held.iter().rposition(same) {
            held.remove(index);
        }
    }
    held.into_iter()
        .rev()
        .map(|mut key| {
            key.dwFlags |= KEYEVENTF_KEYUP;
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: key },
            }
        })
        .collect()
}

/// Release every inserted key-down whose matching key-up was not inserted.
pub(crate) fn release_inserted_keys(events: &[INPUT], inserted: usize) -> bool {
    let releases = inserted_key_releases(events, inserted);
    releases.is_empty()
        || unsafe {
            crate::input::send_desktop_input(&releases, std::mem::size_of::<INPUT>() as i32)
        } as usize
            == releases.len()
}

fn insert_unobservable(events: &[INPUT]) -> UnitRead {
    let sent =
        unsafe { crate::input::send_desktop_input(events, std::mem::size_of::<INPUT>() as i32) };
    if sent > 0 {
        crate::focus_return::note_unconfirmed_input();
    }
    if sent as usize != events.len() {
        release_inserted_keys(events, sent as usize);
        UnitRead::Short(sent)
    } else {
        UnitRead::Unobservable
    }
}

/// A consumed pacing marker gates the next batch but is never read proof.
fn paced_marker_observation(before: i16, state: i16) -> Option<UnitRead> {
    ((state & 1) != before && state >= 0).then_some(UnitRead::Unobservable)
}

/// Read evidence is limited to one isolated DOWN/UP pulse per distinct key.
/// Repeated keys and Unicode packets have no distinct per-unit state identity
/// in a single insertion. Neither case may authorize foreground restoration.
fn unique_pulse_keys(keys: &[VIRTUAL_KEY]) -> bool {
    keys.iter()
        .enumerate()
        .all(|(index, key)| key.0 != 0 && !keys[..index].contains(key))
}

/// A bounded chord has distinct keys, all DOWNs first and their matching UPs
/// in reverse order. Other streams keep their original insertion and remain
/// unobservable rather than being reordered to manufacture a witness.
fn chord_read_split(events: &[INPUT], keys: &[VIRTUAL_KEY]) -> Option<usize> {
    if keys.is_empty() || !unique_pulse_keys(keys) || events.len() != keys.len() * 2 {
        return None;
    }
    let split = keys.len();
    let mut paired = events[..split].iter().zip(events[split..].iter().rev());
    paired
        .all(|(down, up)| {
            if down.r#type != INPUT_KEYBOARD || up.r#type != INPUT_KEYBOARD {
                return false;
            }
            let down = unsafe { down.Anonymous.ki };
            let up = unsafe { up.Anonymous.ki };
            down.dwFlags.0 & (KEYEVENTF_KEYUP.0 | KEYEVENTF_UNICODE.0) == 0
                && up.dwFlags.0 & KEYEVENTF_KEYUP.0 != 0
                && down.wVk == up.wVk
                && down.wScan == up.wScan
                && (up.dwFlags.0 & !KEYEVENTF_KEYUP.0) == down.dwFlags.0
        })
        .then_some(split)
}
pub(crate) fn service_own_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_NOREMOVE};
    let mut message = MSG::default();
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
}

pub(crate) fn keyboard_state() -> Option<[u8; 256]> {
    let mut state = [0u8; 256];
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState(&mut state) }
        .is_ok()
        .then_some(state)
}
/// The caller's thread attached to the foreground thread's input queue for
/// the duration of one foreground input transaction; detaches on drop.
///
/// Attaching makes this thread part of the target's input processing: the
/// two threads share one input queue, and Windows processes that queue in
/// order. Anything it queues for this thread (a sent message, or an event
/// for this thread in the shared queue) sits ahead of the target's keys
/// until this thread retrieves it. A thread that
/// holds the attachment and only answers sent messages therefore freezes the
/// target it waits on: the target stays responsive to sent messages but
/// reads no input until the attachment is dropped (release gate run
/// 45363565804). Every wait below goes through [`Self::idle`], which pumps
/// this thread's own queue completely, as `AttachThreadInput` requires of
/// every attached thread. Windows only returns this thread's own messages
/// there; input routed to the target's windows stays with the target. Input
/// routed to this thread instead (it owns no window, so it should never get
/// any) is counted in `misrouted_input` and never reported as read.
///
/// Attach only once the target thread has processed its activation
/// ([`thread_activation_processed`]): a pending activation in the shared
/// queue leaves it without a focus window, so keys and the activation itself
/// wait on this thread, which never reads input.
pub(crate) struct InputQueueAttachment {
    own_thread: u32,
    target_thread: u32,
    serviced: std::cell::Cell<u32>,
    /// Messages this thread retrieved from its own queue, from just before
    /// attaching.
    pumped: std::cell::Cell<u32>,
    /// Input messages Windows routed to this thread instead of the target.
    misrouted_input: std::cell::Cell<u32>,
    /// Records the messages other threads send this thread while attached,
    /// so a stall names them.
    sent_log: Option<SentMessageLog>,
    /// This thread's `GetQueueStatus` before and right after attaching.
    queue_before: u32,
    queue_attached: u32,
}

/// The calling thread's queue status (high word: kinds queued now; low word:
/// kinds added since the last check), for stall evidence.
fn own_queue_status() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetQueueStatus, QS_ALLINPUT};
    unsafe { GetQueueStatus(QS_ALLINPUT) }
}

/// Retrieve and dispatch every message Windows has for the calling thread,
/// bounded per call. `PeekMessage` with no filter also delivers sent
/// messages and processes the events queued for this thread; it never
/// returns input that belongs to another thread's window. Input it does
/// return was routed to this thread, is not dispatched (this thread has no
/// window that wants it), and is counted in `misrouted_input`.
fn pump_own_queue(pumped: &std::cell::Cell<u32>, misrouted_input: &std::cell::Cell<u32>) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, MSG, PM_REMOVE, WM_INPUT, WM_KEYFIRST, WM_KEYLAST,
        WM_MOUSEFIRST, WM_MOUSELAST,
    };
    for _ in 0..64 {
        let mut msg = MSG::default();
        if !unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            return;
        }
        pumped.set(pumped.get().saturating_add(1));
        let input = (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message)
            || (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg.message)
            || msg.message == WM_INPUT;
        if input {
            misrouted_input.set(misrouted_input.get().saturating_add(1));
            continue;
        }
        unsafe {
            DispatchMessageW(&msg);
        }
    }
}

thread_local! {
    /// (message, receiving window class) -> count, filled by
    /// [`record_sent_message`] while a [`SentMessageLog`] is installed.
    static SENT_MESSAGES: std::cell::RefCell<std::collections::BTreeMap<(u32, String), u32>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

unsafe extern "system" fn record_sent_message(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, CWPSTRUCT};
    // HC_ACTION is 0; wParam is zero when another thread sent the message.
    if code == 0 && wparam.0 == 0 && lparam.0 != 0 {
        let cwp = &*(lparam.0 as *const CWPSTRUCT);
        let class = class_name(cwp.hwnd).unwrap_or_else(|| format!("{:?}", cwp.hwnd.0));
        SENT_MESSAGES.with(|log| {
            *log.borrow_mut().entry((cwp.message, class)).or_default() += 1;
        });
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// A `WH_CALLWNDPROC` hook on the calling thread only (no DLL, no other
/// process): it observes the sent messages this thread dispatches.
struct SentMessageLog(windows::Win32::UI::WindowsAndMessaging::HHOOK);

impl SentMessageLog {
    fn install() -> Option<Self> {
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_CALLWNDPROC};
        SENT_MESSAGES.with(|log| log.borrow_mut().clear());
        unsafe {
            SetWindowsHookExW(
                WH_CALLWNDPROC,
                Some(record_sent_message),
                None,
                GetCurrentThreadId(),
            )
        }
        .ok()
        .map(Self)
    }

    /// The most frequent sent messages, as `0xMSG to Class xN`.
    fn summary() -> String {
        SENT_MESSAGES.with(|log| {
            let log = log.borrow();
            let mut entries: Vec<_> = log.iter().collect();
            entries.sort_by(|a, b| b.1.cmp(a.1));
            entries
                .iter()
                .take(4)
                .map(|((message, class), count)| format!("0x{message:04x} to {class} x{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
}

impl Drop for SentMessageLog {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(self.0) };
    }
}

/// The calling thread's COM apartment, for stall evidence.
fn own_apartment() -> &'static str {
    use windows::Win32::System::Com::{
        CoGetApartmentType, APTTYPE, APTTYPEQUALIFIER, APTTYPE_MAINSTA, APTTYPE_MTA, APTTYPE_NA,
        APTTYPE_STA,
    };
    let mut kind = APTTYPE(0);
    let mut qualifier = APTTYPEQUALIFIER(0);
    match unsafe { CoGetApartmentType(&mut kind, &mut qualifier) } {
        Err(_) => "none",
        Ok(()) if kind == APTTYPE_STA || kind == APTTYPE_MAINSTA => "STA",
        Ok(()) if kind == APTTYPE_MTA => "MTA",
        Ok(()) if kind == APTTYPE_NA => "NA",
        Ok(()) => "other",
    }
}

impl InputQueueAttachment {
    pub(crate) fn attach(foreground: HWND) -> Option<Self> {
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        let target_thread = unsafe { GetWindowThreadProcessId(foreground, None) };
        let own_thread = unsafe { GetCurrentThreadId() };
        if target_thread == 0 || target_thread == own_thread {
            return None;
        }
        // Retrieve what is already queued for this thread (this thread just
        // shared the previous foreground thread's queue for the handoff in
        // `force_foreground_attached`) before it joins the target's queue.
        let pumped = std::cell::Cell::new(0);
        pump_own_queue(&pumped, &std::cell::Cell::new(0));
        let before = own_queue_status();

        unsafe { AttachThreadInput(own_thread, target_thread, true) }
            .as_bool()
            .then(|| Self {
                own_thread,
                target_thread,
                serviced: std::cell::Cell::new(0),
                pumped,
                misrouted_input: std::cell::Cell::new(0),
                sent_log: SentMessageLog::install(),
                queue_before: before,
                queue_attached: own_queue_status(),
            })
    }

    pub(crate) fn matches_foreground(&self, foreground: HWND) -> bool {
        self.target_thread == unsafe { GetWindowThreadProcessId(foreground, None) }
    }

    /// Pump this thread's own share of the attached queue: sent messages,
    /// queued events and posted messages. Counts the steps that found a sent
    /// message waiting.
    fn pump(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{GetQueueStatus, QS_SENDMESSAGE};
        let pending = unsafe { GetQueueStatus(QS_SENDMESSAGE) } >> 16 & QS_SENDMESSAGE.0;
        if pending != 0 {
            self.serviced.set(self.serviced.get().saturating_add(1));
        }
        pump_own_queue(&self.pumped, &self.misrouted_input);
    }

    /// Whether the target read the inserted sentinel: its toggle bit in the
    /// shared key state flipped, and no input went to this thread instead
    /// (this thread reading the sentinel would flip it too).
    #[cfg(test)]
    fn read_by_target(&self, toggled: bool) -> bool {
        toggled && self.misrouted_input.get() == 0
    }

    /// Whether no input was removed from the shared queue by this thread.
    pub(crate) fn has_no_misrouted_input(&self) -> bool {
        self.misrouted_input.get() == 0
    }

    /// One bounded wait step while attached: pump this thread's queue first.
    pub(crate) fn idle(&self, step: Duration) {
        self.pump();
        if step.is_zero() {
            std::thread::yield_now();
        } else {
            sleep(step);
        }
    }

    /// Diagnostic for a stall: how many times this thread had to answer a
    /// message sent to it while attached, and whether the target thread
    /// answers a WM_NULL within 200 ms.
    fn stall_detail(&self, target: HWND) -> String {
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL,
        };
        let responsive = unsafe {
            SendMessageTimeoutW(
                target,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                200,
                None,
            )
        }
        .0 != 0;
        let (active, focus) = gui_thread_active_and_focus(target).unwrap_or_default();
        let sent = if self.sent_log.is_some() {
            SentMessageLog::summary()
        } else {
            "not recorded".to_owned()
        };
        format!(
            "Driver thread ({apartment}) answered {} sent-message batch(es) [{sent}] and \
             retrieved {} message(s) of its own ({} of them input meant for the target) while \
             attached; target thread {} WM_NULL within 200 ms; its queue's active HWND {:?}, \
             focus HWND {:?}, caption {}; driver queue status 0x{:08x} before attach, \
             0x{:08x} attached, 0x{:08x} now.",
            self.serviced.get(),
            self.pumped.get(),
            self.misrouted_input.get(),
            if responsive {
                "answered"
            } else {
                "did not answer"
            },
            active.0,
            focus.0,
            if caption_active(target) {
                "active"
            } else {
                "inactive"
            },
            self.queue_before,
            self.queue_attached,
            own_queue_status(),
            apartment = own_apartment(),
        )
    }

    /// Pace an otherwise unobservable text batch behind a queue marker.
    /// Reading the marker only gates the next batch; it never proves that the
    /// preceding input reached the target or permits foreground restoration.
    fn send_paced_unobservable(
        &self,
        events: &[INPUT],
        timeout: Duration,
        check_reader: &impl Fn(bool) -> Result<()>,
    ) -> Result<UnitRead> {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
        const DRAIN_MARKER: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
        let marker_state = unsafe { GetKeyState(DRAIN_MARKER.0 as i32) };
        if marker_state < 0 {
            return Err(ForegroundInputError {
                error: anyhow::anyhow!("input_read_unobservable: queue marker is already held; no text from this batch was sent"),
                turn: TypingTurn::NotSent,
            }.into());
        }
        let before = marker_state & 1;
        let marker = |up| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: DRAIN_MARKER,
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut pending = events.to_vec();
        pending.extend([marker(false), marker(true)]);
        let sent = unsafe {
            crate::input::send_desktop_input(&pending, std::mem::size_of::<INPUT>() as i32)
        } as usize;
        if sent > 0 {
            crate::focus_return::note_unconfirmed_input();
        }
        if sent != pending.len() {
            release_inserted_keys(&pending, sent);
            return Ok(if sent < events.len() {
                UnitRead::Short(sent as u32)
            } else {
                UnitRead::Stalled
            });
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            check_reader(true)?;
            if !self.has_no_misrouted_input() {
                return Err(ForegroundInputError {
                    error: anyhow::anyhow!("input_read_unobservable: input was delivered to the driver thread; remaining input was not sent"),
                    turn: TypingTurn::Unobservable,
                }.into());
            }
            let state = unsafe { GetKeyState(DRAIN_MARKER.0 as i32) };
            if let Some(result) = paced_marker_observation(before, state) {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    "text queue pacing stalled: {}",
                    self.stall_detail(unsafe { GetForegroundWindow() })
                );
                return Ok(UnitRead::Stalled);
            }
            sleep(Duration::from_millis(1));
        }
    }

    /// Observe the requested chord's DOWN and UP states on its attached reader.
    /// UP is inserted only after DOWN was observed. An unrelated message or
    /// another reader's state never acknowledges the inserted prefix.
    fn send_and_await_read(
        &self,
        events: &[INPUT],
        keys: Option<&[VIRTUAL_KEY]>,
        target: ForegroundTarget,
        timeout: Duration,
        pace_unobservable: bool,
    ) -> Result<UnitRead> {
        let target_closed = std::cell::Cell::new(false);
        let check_reader = |prefix_inserted: bool| -> Result<()> {
            service_own_messages();
            if let Err(error) = crate::human_input::check() {
                return Err(ForegroundInputError {
                    error: anyhow::anyhow!(error),
                    turn: TypingTurn::Unobservable,
                }
                .into());
            }
            let foreground = unsafe { GetForegroundWindow() };
            // Dismissing a toolkit dialog may hide it before destruction. Only
            // an already inserted chord may finish its owned UPs on the known
            // same-thread successor; no new DOWN is allowed on that successor.
            let matches_target =
                foreground_matches_target_or_owned_window(target, foreground.0 as usize as u64);
            let closed_successor = prefix_inserted
                && crate::win32::foreground_after_target_closed(
                    target,
                    foreground.0 as usize as u64,
                );
            // Hide can clear visibility while this exact HWND is still the
            // foreground on the same receiver thread. Complete only its owned
            // UPs, without treating that transition as a read receipt.
            let hidden_target = prefix_inserted
                && !matches_target
                && target.is_same_live_window(foreground.0 as usize as u64);
            if !matches_target && !closed_successor && !hidden_target {
                return Err(ForegroundInputError {
                    error: anyhow::anyhow!(
                        "foreground_lost: stopped before another key phase \
                         (target={target:?}, actual={foreground:?}, \
                         prefix_inserted={prefix_inserted}, \
                         receiver_thread_unchanged={}, closed_successor={closed_successor})",
                        self.matches_foreground(foreground)
                    ),
                    turn: TypingTurn::Lost,
                }
                .into());
            }
            if (closed_successor || hidden_target) && !target_closed.replace(true) {
                crate::focus_return::note_unconfirmed_input();
            }
            if !self.matches_foreground(foreground) {
                return Err(ForegroundInputError {
                    error: anyhow::anyhow!(
                        "input_read_unobservable: the foreground receiver changed input thread; \
                         requested input was not replayed"
                    ),
                    turn: TypingTurn::Unobservable,
                }
                .into());
            }
            Ok(())
        };
        check_reader(false)?;
        let observable = keys.filter(|keys| {
            chord_read_split(events, keys).is_some()
                && keyboard_state()
                    .is_some_and(|state| keys.iter().all(|key| state[key.0 as usize] & 0x80 == 0))
        });
        let Some(keys) = observable else {
            if !pace_unobservable {
                return Ok(insert_unobservable(events));
            }
            return self.send_paced_unobservable(events, timeout, &check_reader);
        };
        let split = keys.len();
        let deadline = Instant::now() + timeout;
        let sent = unsafe {
            crate::input::send_desktop_input(&events[..split], std::mem::size_of::<INPUT>() as i32)
        };
        if sent as usize != split {
            release_inserted_keys(events, sent as usize);
            return Ok(UnitRead::Short(sent));
        }
        let wait_for = |down: bool| -> Result<Option<bool>> {
            loop {
                self.pump();
                check_reader(true)?;
                if self.misrouted_input.get() != 0 {
                    return Ok(None);
                }
                let Some(state) = keyboard_state() else {
                    return Ok(None);
                };
                if keys
                    .iter()
                    .all(|key| (state[key.0 as usize] & 0x80 != 0) == down)
                {
                    return Ok(Some(true));
                }
                if Instant::now() >= deadline {
                    return Ok(Some(false));
                }
                sleep(Duration::from_millis(1));
            }
        };
        let down_read = match wait_for(true) {
            Ok(Some(false)) => {
                release_inserted_keys(events, split);
                return Ok(UnitRead::Stalled);
            }
            Ok(read) => read,
            Err(error) => {
                release_inserted_keys(events, split);
                return Err(error);
            }
        };
        if let Err(error) = check_reader(true) {
            release_inserted_keys(events, split);
            return Err(error);
        }
        let sent = unsafe {
            crate::input::send_desktop_input(&events[split..], std::mem::size_of::<INPUT>() as i32)
        };
        if sent as usize != events.len() - split {
            release_inserted_keys(events, split + sent as usize);
            return Ok(UnitRead::Short(split as u32 + sent));
        }
        if down_read.is_none() {
            return Ok(UnitRead::Unobservable);
        }
        let read = match wait_for(false)? {
            Some(true) => UnitRead::Read,
            Some(false) => UnitRead::Stalled,
            None => UnitRead::Unobservable,
        };
        // A hidden target's successor never supplies a restoration receipt.
        Ok(if target_closed.get() && matches!(read, UnitRead::Read) {
            UnitRead::Unobservable
        } else {
            read
        })
    }
}

/// Whether `hwnd`'s frame is drawn active: set when its thread handles
/// `WM_NCACTIVATE(TRUE)` for a foreground activation.
fn caption_active(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowInfo, WINDOWINFO};
    let mut info = WINDOWINFO {
        cbSize: std::mem::size_of::<WINDOWINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowInfo(hwnd, &mut info) }.is_ok() && info.dwWindowStatus & 1 != 0
}

impl Drop for InputQueueAttachment {
    fn drop(&mut self) {
        self.sent_log = None;
        use windows::Win32::System::Threading::AttachThreadInput;
        let _ = unsafe { AttachThreadInput(self.own_thread, self.target_thread, false) };
    }
}

enum TextUnit {
    Packet(u16),
    /// A layout keystroke for a character, with Shift held when the layout needs it.
    Key {
        vk: VIRTUAL_KEY,
        shift: bool,
    },
    Return,
    Tab,
}

/// The planner's view of `units`: a high surrogate stays with its low one,
/// and Return and Tab are batch boundaries.
fn unit_kinds(units: &[TextUnit]) -> Vec<type_pacing::UnitKind> {
    use type_pacing::UnitKind;
    units
        .iter()
        .map(|unit| match unit {
            TextUnit::Return | TextUnit::Tab => UnitKind::Key,
            TextUnit::Packet(0xD800..=0xDBFF) => UnitKind::Lead,
            TextUnit::Packet(_) | TextUnit::Key { .. } => UnitKind::Text,
        })
        .collect()
}

/// Type characters as real keystrokes where the target's keyboard layout has them without AltGr: consoles such as
/// PowerShell's PSReadLine ignore VK_PACKET input. Other characters, and surrogate pairs, stay Unicode packets.
fn keyed(
    units: Vec<TextUnit>,
    layout: windows::Win32::UI::Input::KeyboardAndMouse::HKL,
    caps_lock: bool,
) -> Vec<TextUnit> {
    use windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanExW;
    units
        .into_iter()
        .map(|unit| match unit {
            TextUnit::Packet(code) if !(0xD800..0xE000).contains(&code) && code >= 0x20 => {
                let scan = unsafe { VkKeyScanExW(code, layout) } as u16;
                let (vk, mods) = (scan & 0xFF, scan >> 8);
                // -1 means unmapped; Ctrl/Alt (AltGr) combinations stay packets.
                if scan == 0xFFFF || mods & !1 != 0 {
                    return TextUnit::Packet(code);
                }
                let letter = char::from_u32(code as u32).is_some_and(char::is_alphabetic);
                TextUnit::Key {
                    vk: VIRTUAL_KEY(vk),
                    shift: (mods & 1 == 1) != (caps_lock && letter),
                }
            }
            other => other,
        })
        .collect()
}

/// One readable VK_PACKET, or one Return. A supplementary scalar is two Packets (high, low).
fn text_units(text: &str) -> Vec<TextUnit> {
    let mut units = Vec::with_capacity(text.len());
    let mut prev_was_cr = false;
    for ch in text.chars() {
        match ch {
            '\n' if prev_was_cr => {
                prev_was_cr = false;
            }
            '\n' | '\r' => {
                units.push(TextUnit::Return);
                prev_was_cr = ch == '\r';
            }
            '\t' => {
                prev_was_cr = false;
                units.push(TextUnit::Tab);
            }
            _ => {
                prev_was_cr = false;
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    units.push(TextUnit::Packet(*unit));
                }
            }
        }
    }
    units
}

/// The unit's own key events, Shift wrapped around a shifted keystroke.
fn unit_keys(unit: &TextUnit) -> Vec<INPUT> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_RETURN, VK_SHIFT, VK_TAB};
    match unit {
        TextUnit::Packet(c) => vec![unicode_key_input(*c, false), unicode_key_input(*c, true)],
        TextUnit::Return => vec![key_input(VK_RETURN, false), key_input(VK_RETURN, true)],
        TextUnit::Tab => vec![key_input(VK_TAB, false), key_input(VK_TAB, true)],
        TextUnit::Key { vk, shift: false } => vec![key_input(*vk, false), key_input(*vk, true)],
        TextUnit::Key { vk, shift: true } => vec![
            key_input(VK_SHIFT, false),
            key_input(*vk, false),
            key_input(*vk, true),
            key_input(VK_SHIFT, true),
        ],
    }
}

/// The unit's requested key events, with no appended helper input.
#[cfg(test)]
fn unit_events(unit: &TextUnit) -> Vec<INPUT> {
    batch_events(std::slice::from_ref(unit))
}

/// The distinct requested pulse identities of a batch.
fn batch_pulse_keys(units: &[TextUnit]) -> Option<Vec<VIRTUAL_KEY>> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_RETURN, VK_SHIFT, VK_TAB};
    let mut keys = Vec::new();
    for unit in units {
        match unit {
            TextUnit::Packet(_) => return None,
            TextUnit::Return => keys.push(VK_RETURN),
            TextUnit::Tab => keys.push(VK_TAB),
            TextUnit::Key { vk, shift } => {
                if *shift {
                    keys.push(VK_SHIFT);
                }
                keys.push(*vk);
            }
        }
    }
    unique_pulse_keys(&keys).then_some(keys)
}

/// One batch's requested key events, preserving their order.
fn batch_events(units: &[TextUnit]) -> Vec<INPUT> {
    let mut events = Vec::with_capacity(units.len() * 4);
    for unit in units {
        events.extend(unit_keys(unit));
    }
    events
}

/// Build a single Unicode keyboard INPUT struct for one UTF-16 code unit,
/// either down (`up = false`) or up (`up = true`). Used by
/// [`send_text_synthesized`].
fn unicode_key_input(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Build a single keyboard INPUT struct for `vk`, either down (`up = false`)
/// or up (`up = true`). Uses scancode + EXTENDEDKEY where applicable so the
/// target sees a hardware-like keystroke.
pub(crate) fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) };
    scanned_key_input(vk, scan, up)
}

fn scanned_key_input(vk: VIRTUAL_KEY, scan: u32, up: bool) -> INPUT {
    let scan = scan as u16;
    let mut flags: KEYBD_EVENT_FLAGS = KEYBD_EVENT_FLAGS(0);
    // Scancode is more reliable than VK for some apps. EXTENDEDKEY flag
    // makes arrow / nav / right-side modifier keys work correctly.
    if scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: if scan != 0 { VIRTUAL_KEY(0) } else { vk },
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn modifier_vk(name: &str) -> Option<VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    match name.to_lowercase().as_str() {
        "ctrl" | "control" => Some(VK_CONTROL),
        "shift" => Some(VK_SHIFT),
        "alt" | "menu" | "option" => Some(VK_MENU),
        "win" | "meta" | "windows" | "cmd" | "command" => Some(VK_LWIN),
        _ => None,
    }
}

/// Build the SendInput `INPUT` events that HOLD the named modifier keys around a
/// pointer action: `(downs, ups)` where `downs` presses each modifier (in order)
/// and `ups` releases them (reverse order). A pointer path emits `downs`, does
/// the click, then emits `ups`. Unknown modifier names are skipped. Empty input
/// → two empty vecs (no-op). Lets the mouse SendInput path hold cmd/shift/alt/
/// ctrl exactly like `send_key_synthesized` does for keystrokes.
pub fn modifier_hold_inputs(modifiers: &[&str]) -> (Vec<INPUT>, Vec<INPUT>) {
    let mod_vks: Vec<VIRTUAL_KEY> = modifiers.iter().filter_map(|m| modifier_vk(m)).collect();
    let downs: Vec<INPUT> = mod_vks.iter().map(|v| key_input(*v, false)).collect();
    let ups: Vec<INPUT> = mod_vks.iter().rev().map(|v| key_input(*v, true)).collect();
    (downs, ups)
}

fn is_extended(vk: VIRTUAL_KEY) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    matches!(
        vk,
        VK_DELETE
            | VK_INSERT
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_UP
            | VK_DOWN
            | VK_LEFT
            | VK_RIGHT
            | VK_RCONTROL
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
            | VK_NUMLOCK
            | VK_SNAPSHOT
    )
}

/// A key press resolved for one target window.
enum Press {
    /// A virtual key and its scan code on the layout that translates it.
    Key { vk: VIRTUAL_KEY, scan: u32 },
    /// A character the target's layout cannot type with one unshifted key;
    /// it goes through `type_text`'s Unicode path instead.
    Text(char),
}

/// A single printable character with no modifiers must type that character,
/// so it is resolved on the target thread's keyboard layout (#6181): on Thai
/// Kedmanee VK_3 types '-', and on AZERTY '3' needs Shift. Named keys and
/// chords such as ctrl+s stay on their virtual keys.
fn resolve_press(target: HWND, key: &str, modifiers: &[&str]) -> Result<Press> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardLayout, MapVirtualKeyExW, VkKeyScanExW,
    };
    let mut chars = key.chars();
    if let (Some(ch), None, true) = (chars.next(), chars.next(), modifiers.is_empty()) {
        if !ch.is_control() {
            // Letter names are case-insensitive, as on the other platforms:
            // "A" types what "a" types and does not add Shift.
            let ch = ch.to_ascii_lowercase();
            let hkl = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(target, None)) };
            let vk = u16::try_from(ch as u32).ok().and_then(|unit| {
                crate::keycodes::unshifted_layout_key(unsafe { VkKeyScanExW(unit, hkl) })
            });
            return Ok(match vk {
                Some(vk) => Press::Key {
                    vk: VIRTUAL_KEY(vk),
                    scan: unsafe { MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC, Some(hkl)) },
                },
                None => Press::Text(ch),
            });
        }
    }
    let vk = key_name_to_vk(key)?;
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) };
    Ok(Press::Key { vk, scan })
}

pub(crate) fn key_name_to_vk(key: &str) -> Result<VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    // Windows reserves the ASCII values themselves as the virtual-key codes
    // for A-Z and 0-9. Resolve the documented alphanumeric vocabulary
    // directly so it does not depend on the daemon thread having a current
    // keyboard layout, which VkKeyScanW requires.
    if let Some(vk) = crate::keycodes::ascii_alphanumeric_virtual_key_code(key) {
        return Ok(VIRTUAL_KEY(vk));
    }

    let vk = match key.to_lowercase().as_str() {
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "escape" | "esc" => VK_ESCAPE,
        "space" | " " => VK_SPACE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        "ctrl" | "control" => VK_CONTROL,
        "shift" => VK_SHIFT,
        "alt" => VK_MENU,
        "win" | "windows" | "meta" | "command" | "cmd" => VK_LWIN,
        "capslock" => VK_CAPITAL,
        "numlock" => VK_NUMLOCK,
        _ => {
            // Single printable character.
            let ch = key
                .chars()
                .next()
                .ok_or_else(|| anyhow::anyhow!("Empty key name"))?;
            // VkKeyScanW returns VK in low byte.
            let vk_scan =
                unsafe { windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW(ch as u16) };
            if vk_scan == -1i16 as u16 as i16 {
                bail!("Unknown key: {key}");
            }
            VIRTUAL_KEY((vk_scan & 0xFF) as u16)
        }
    };
    Ok(vk)
}

#[cfg(test)]
mod extended_key_tests {
    use super::*;

    #[test]
    fn windows_key_presses_and_releases_carry_the_extended_key_flag() {
        // The left Windows key's scan code is 0xE05B; without the extended
        // flag, Windows treats it as an unrelated key and shortcuts such as
        // Win+S do nothing.
        let (downs, ups) = modifier_hold_inputs(&["win"]);
        assert_eq!((downs.len(), ups.len()), (1, 1));
        for input in downs.iter().chain(&ups) {
            let flags = unsafe { input.Anonymous.ki.dwFlags };
            assert_ne!(flags.0 & KEYEVENTF_EXTENDEDKEY.0, 0, "{flags:?}");
        }
    }
}

#[cfg(test)]
mod text_unit_tests {
    use super::*;
    #[test]
    fn pacing_marker_must_be_released_and_never_confirms_input_for_focus_return() {
        assert!(paced_marker_observation(0, 0).is_none());
        assert!(paced_marker_observation(0, i16::MIN | 1).is_none());
        let drained = paced_marker_observation(0, 1).expect("marker down and up were read");
        assert!(matches!(drained, UnitRead::Unobservable));
        assert!(!restores_previous(drained.turn()));
        let drained_again = paced_marker_observation(1, 0).expect("next marker toggled back");
        assert!(!restores_previous(drained_again.turn()));
    }

    #[test]
    fn partial_batch_releases_only_outstanding_requested_and_marker_keys() {
        let key = |vk, up| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let events = [
            key(0x41, false),
            key(0x41, true),
            key(0xE8, false),
            key(0xE8, true),
        ];
        assert!(inserted_key_releases(&events, 0).is_empty());
        let requested = inserted_key_releases(&events, 1);
        assert_eq!(requested.len(), 1);
        assert_eq!(unsafe { requested[0].Anonymous.ki }.wVk, VIRTUAL_KEY(0x41));
        assert!(inserted_key_releases(&events, 2).is_empty());
        let marker = inserted_key_releases(&events, 3);
        assert_eq!(marker.len(), 1);
        let marker_up = unsafe { marker[0].Anonymous.ki };
        assert_eq!(marker_up.wVk, VIRTUAL_KEY(0xE8));
        assert_ne!(marker_up.dwFlags.0 & KEYEVENTF_KEYUP.0, 0);
        assert!(inserted_key_releases(&events, 4).is_empty());
    }

    #[test]
    fn pulse_read_coverage_rejects_repeated_keys_and_unicode_packets() {
        let key = |vk, shift| TextUnit::Key {
            vk: VIRTUAL_KEY(vk),
            shift,
        };
        assert_eq!(
            batch_pulse_keys(&[key(0x41, false), key(0x42, false)]),
            Some(vec![VIRTUAL_KEY(0x41), VIRTUAL_KEY(0x42)])
        );
        assert_eq!(
            batch_pulse_keys(&[key(0x41, false), key(0x41, false)]),
            None
        );
        // Distinct characters can still repeat the Shift pulse in one batch.
        assert_eq!(batch_pulse_keys(&[key(0x41, true), key(0x42, true)]), None);
        assert_eq!(batch_pulse_keys(&[TextUnit::Packet(0x4E8C)]), None);
        assert_eq!(
            batch_pulse_keys(&[TextUnit::Return, TextUnit::Return]),
            None
        );
        assert!(!unique_pulse_keys(&[VIRTUAL_KEY(0)]));
    }

    #[test]
    fn only_a_bounded_chord_can_observe_down_before_inserting_up() {
        let key = |vk, up| scanned_key_input(VIRTUAL_KEY(vk), 0, up);
        let keys = [VIRTUAL_KEY(0x11), VIRTUAL_KEY(0x41)];
        assert_eq!(
            chord_read_split(
                &[
                    key(0x11, false),
                    key(0x41, false),
                    key(0x41, true),
                    key(0x11, true)
                ],
                &keys
            ),
            Some(2)
        );
        assert_eq!(
            chord_read_split(
                &[
                    key(0x11, false),
                    key(0x11, true),
                    key(0x41, false),
                    key(0x41, true)
                ],
                &keys
            ),
            None
        );
        assert_eq!(
            chord_read_split(&[key(0x41, false), key(0x42, true)], &[keys[1]]),
            None
        );
        assert_eq!(
            chord_read_split(&unit_keys(&TextUnit::Packet(0x4E8C)), &[keys[1]]),
            None
        );
        assert_eq!(chord_read_split(&[], &[]), None);
    }
    #[test]
    fn cjk_issue_fixture_is_three_packets() {
        assert!(matches!(
            text_units("二等座").as_slice(),
            [
                TextUnit::Packet(0x4E8C),
                TextUnit::Packet(0x7B49),
                TextUnit::Packet(0x5EA7)
            ]
        ));
    }

    #[test]
    fn emoji_is_two_surrogate_packets() {
        assert!(matches!(
            text_units("🙂").as_slice(),
            [TextUnit::Packet(0xD83D), TextUnit::Packet(0xDE42)]
        ));
    }

    #[test]
    fn crlf_and_lone_breaks_are_one_return_each() {
        assert!(matches!(
            text_units("a\r\nb\rc\nd").as_slice(),
            [
                TextUnit::Packet(0x61),
                TextUnit::Return,
                TextUnit::Packet(0x62),
                TextUnit::Return,
                TextUnit::Packet(0x63),
                TextUnit::Return,
                TextUnit::Packet(0x64),
            ]
        ));
    }

    #[test]
    fn newline_then_cr_is_two_returns() {
        assert!(matches!(
            text_units("\n\r").as_slice(),
            [TextUnit::Return, TextUnit::Return]
        ));
    }

    #[test]
    fn tab_is_a_key_of_its_own() {
        assert!(matches!(
            text_units("a\tb").as_slice(),
            [
                TextUnit::Packet(0x61),
                TextUnit::Tab,
                TextUnit::Packet(0x62)
            ]
        ));
        for input in &unit_events(&TextUnit::Tab)[..2] {
            let flags = unsafe { input.Anonymous.ki.dwFlags };
            assert_eq!(flags.0 & KEYEVENTF_UNICODE.0, 0);
        }
    }

    #[test]
    fn batches_never_split_a_surrogate_pair_or_cross_a_key() {
        let text = format!(
            "{}\u{1F600}\n{}\t{}",
            "a".repeat(15),
            "b".repeat(40),
            "\u{1F642}".repeat(9)
        );
        let units = text_units(&text);
        let kinds = unit_kinds(&units);
        let mut next = 0;
        for range in type_pacing::plan_chunks(&kinds, type_pacing::BATCH_UNITS) {
            assert_eq!(range.start, next);
            next = range.end;
            let batch = &units[range];
            assert!(batch.len() <= type_pacing::BATCH_UNITS);
            // A batch holds either one key or only packets, and ends on a
            // pair boundary.
            let keys = batch
                .iter()
                .filter(|u| !matches!(u, TextUnit::Packet(_)))
                .count();
            assert!(keys == 0 || batch.len() == 1);
            if let Some(TextUnit::Packet(last)) = batch.last() {
                assert!(!(0xD800..=0xDBFF).contains(last));
            }
            // Only the requested Unicode pairs are inserted.
            assert_eq!(batch_events(batch).len(), batch.len() * 2);
        }
        assert_eq!(next, units.len());
    }

    #[test]
    fn empty_text_has_no_units() {
        assert!(text_units("").is_empty());
    }

    #[test]
    fn packet_events_are_only_the_requested_unicode_pair() {
        let events = unit_events(&TextUnit::Packet(0x4E8C));
        let keys: Vec<_> = events
            .iter()
            .map(|input| unsafe { input.Anonymous.ki })
            .collect();
        assert_eq!(keys[0].wVk.0, 0);
        assert_eq!(keys[1].wVk.0, 0);
        assert_ne!(keys[0].dwFlags.0 & KEYEVENTF_UNICODE.0, 0);
        assert_ne!(keys[1].dwFlags.0 & KEYEVENTF_UNICODE.0, 0);
        assert_eq!(keys[0].dwFlags.0 & KEYEVENTF_KEYUP.0, 0);
        assert_ne!(keys[1].dwFlags.0 & KEYEVENTF_KEYUP.0, 0);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn a_shifted_key_holds_shift_around_only_the_requested_pulse() {
        use windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT;
        let events = unit_events(&TextUnit::Key {
            vk: VIRTUAL_KEY(0xBD),
            shift: true,
        });
        let keys: Vec<_> = events
            .iter()
            .map(|input| unsafe { input.Anonymous.ki })
            .collect();
        // key_input sends scan codes, including the held Shift.
        let scan = |vk: u16| unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        assert_eq!(
            keys[..4].iter().map(|key| key.wScan).collect::<Vec<_>>(),
            [scan(VK_SHIFT.0), scan(0xBD), scan(0xBD), scan(VK_SHIFT.0)]
        );
        assert_eq!(keys.len(), 4);
        assert!(keys
            .iter()
            .all(|key| key.dwFlags.0 & KEYEVENTF_UNICODE.0 == 0));
        assert_eq!(
            keys.iter()
                .map(|key| key.dwFlags.0 & KEYEVENTF_KEYUP.0 != 0)
                .collect::<Vec<_>>(),
            [false, false, true, true]
        );
    }

    #[test]
    fn return_events_are_not_unicode() {
        let events = unit_events(&TextUnit::Return);
        for input in &events[..2] {
            let flags = unsafe { input.Anonymous.ki.dwFlags };
            assert_eq!(flags.0 & KEYEVENTF_UNICODE.0, 0);
        }
    }
}

#[cfg(test)]
mod post_text_unit_tests {
    use super::*;

    fn packets(text: &str) -> Vec<Option<u16>> {
        text_units(text)
            .into_iter()
            .map(|u| match u {
                TextUnit::Packet(c) => Some(c),
                TextUnit::Return | TextUnit::Tab | TextUnit::Key { .. } => None,
            })
            .collect()
    }

    #[test]
    fn supplementary_scalar_is_two_surrogate_units() {
        assert_eq!(packets("\u{1F600}"), vec![Some(0xD83D), Some(0xDE00)]);
    }

    #[test]
    fn crlf_is_one_return_and_symbols_survive() {
        assert_eq!(
            packets("a\r\nb\n~é"),
            vec![Some(97), None, Some(98), None, Some(126), Some(0xE9)]
        );
    }
}

/// Micro-benchmark and correctness check for background `WM_CHAR` text
/// delivery into a real EDIT control. Run on a Windows desktop session:
/// `cargo test -p platform-windows --lib bench_post_type_text -- --ignored --nocapture`
#[cfg(test)]
mod type_text_bench {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Instant;
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowTextLengthW, GetWindowTextW,
        PeekMessageW, SetWindowTextW, TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE,
        WINDOW_STYLE, WS_POPUP,
    };

    /// An EDIT control with its own message pump on a worker thread.
    struct Edit {
        hwnd: HWND,
        stop: Arc<AtomicBool>,
        pump: Option<std::thread::JoinHandle<()>>,
    }

    impl Edit {
        fn new() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let pump_stop = stop.clone();
            let (tx, rx) = mpsc::channel();
            let pump = std::thread::spawn(move || unsafe {
                let instance = GetModuleHandleW(None).expect("module handle");
                // ES_MULTILINE (0x4) so a long text is not clipped to one line.
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    w!(""),
                    WINDOW_STYLE(WS_POPUP.0 | 0x0004),
                    0,
                    0,
                    400,
                    200,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
                .expect("create EDIT control");
                tx.send(hwnd.0 as isize).expect("send handle");
                let mut msg = MSG::default();
                while !pump_stop.load(Ordering::Acquire) {
                    if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        // Like a real application loop: a posted WM_KEYDOWN
                        // becomes the WM_CHAR the control inserts.
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    } else {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
                let _ = DestroyWindow(hwnd);
            });
            let hwnd = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("EDIT control created");
            Self {
                hwnd: HWND(hwnd as *mut _),
                stop,
                pump: Some(pump),
            }
        }

        fn text(&self) -> String {
            unsafe {
                let len = GetWindowTextLengthW(self.hwnd);
                let mut buf = vec![0u16; len as usize + 1];
                let got = GetWindowTextW(self.hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..got as usize])
            }
        }

        fn clear(&self) {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, w!(""));
            }
        }
    }

    impl Drop for Edit {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(pump) = self.pump.take() {
                let _ = pump.join();
            }
        }
    }

    #[test]
    #[ignore = "benchmark; needs an interactive Windows desktop session"]
    fn bench_post_type_text() {
        let edit = Edit::new();
        let text = "The quick brown fox jumps over the lazy dog, then types 1234567890 in a field.";
        let n = text.chars().count();
        // 30 is the old default; 0 is the new one.
        for (label, delay_ms) in [
            ("paced (before, delay_ms=30)", 30u64),
            ("batched (after, delay_ms=0)", 0),
        ] {
            let mut runs = Vec::new();
            for _ in 0..9 {
                edit.clear();
                let started = Instant::now();
                post_type_text_with_delay(edit.hwnd.0 as u64, text, delay_ms).expect("post");
                runs.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(
                    edit.text(),
                    text,
                    "{label}: every character landed in order"
                );
            }
            runs.sort_by(|a, b| a.total_cmp(b));
            eprintln!(
                "BENCH wm_char {label} chars={n} p50_ms={:.1} min_ms={:.1} max_ms={:.1}",
                runs[4], runs[0], runs[8]
            );
        }
    }

    #[test]
    #[ignore = "needs an interactive Windows desktop session"]
    fn batched_post_keeps_order_with_a_tab_between_batches() {
        let edit = Edit::new();
        let text = "alpha beta gamma delta epsilon\tzeta eta theta iota kappa lambda";
        post_type_text_with_delay(edit.hwnd.0 as u64, text, 0).expect("post");
        assert_eq!(edit.text(), text);
    }
}

/// A driver thread attached to the target's input queue must answer messages
/// the target sends it while it waits for the target to read input. Before
/// this, the wait spun without servicing sent messages, so a target that sent
/// one froze until the driver detached, read nothing within 2000 ms and the
/// driver reported `text_delivery_stalled` (WPF harness, run 45020095461: the
/// text appeared right after the stall, with the caret frozen during it).
#[cfg(test)]
mod attached_wait_services_sent_messages {
    use super::*;
    use std::sync::mpsc;
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, SendMessageTimeoutW, MSG,
        PM_REMOVE, SMTO_NORMAL, WINDOW_EX_STYLE, WM_NULL, WS_POPUP,
    };

    fn popup() -> HWND {
        unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("create popup")
        }
    }

    #[test]
    #[ignore = "creates windows on the desktop; selected by the Windows integration runner"]
    fn attached_wait_answers_a_message_the_target_sends() {
        // The waiting thread owns a window the target can send to.
        let own = popup();
        let own_addr = own.0 as isize;
        let (target_tx, target_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let target = std::thread::spawn(move || unsafe {
            let hwnd = popup();
            target_tx.send(hwnd.0 as isize).expect("send target hwnd");
            go_rx.recv().expect("start signal");
            let started = Instant::now();
            let answered = SendMessageTimeoutW(
                HWND(own_addr as *mut _),
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_NORMAL,
                3000,
                None,
            )
            .0 != 0;
            done_tx
                .send((answered, started.elapsed()))
                .expect("send result");
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = DestroyWindow(hwnd);
        });
        let target_hwnd = HWND(target_rx.recv().expect("target hwnd") as *mut _);
        let attachment = InputQueueAttachment::attach(target_hwnd).expect("attach input queues");
        go_tx.send(()).expect("start target");

        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Ok(result) = done_rx.try_recv() {
                break Some(result);
            }
            if Instant::now() >= deadline {
                break None;
            }
            attachment.idle(Duration::from_millis(5));
        };
        drop(attachment);
        let (answered, elapsed) = result.unwrap_or_else(|| {
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("target finished")
        });
        target.join().expect("target thread");
        unsafe {
            let _ = DestroyWindow(own);
        }
        assert!(answered, "the target's sent message was never answered");
        assert!(
            elapsed < Duration::from_secs(1),
            "the attached wait answered the target's sent message only after {elapsed:?}"
        );
    }
}

/// While attached, the driver thread must take everything Windows queues for
/// it, not only sent messages: whatever it leaves sits in the shared queue
/// ahead of the target's input (release gate run 45363565804: the target
/// answered WM_NULL yet read none of the 16 inserted units until the driver
/// detached). Input it is handed instead of the target never counts as read.
#[cfg(test)]
mod attached_wait_pumps_own_queue {
    use super::*;
    use std::sync::mpsc;
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, PostThreadMessageW, MSG,
        PM_NOREMOVE, PM_REMOVE, WINDOW_EX_STYLE, WM_APP, WS_POPUP,
    };

    fn popup() -> HWND {
        unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("create popup")
        }
    }

    /// A pumping target thread with one window, alive until `stop` is sent.
    fn target_thread() -> (HWND, mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (hwnd_tx, hwnd_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let join = std::thread::spawn(move || unsafe {
            let hwnd = popup();
            hwnd_tx.send(hwnd.0 as isize).expect("send hwnd");
            while stop_rx.try_recv().is_err() {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                sleep(Duration::from_millis(5));
            }
            let _ = DestroyWindow(hwnd);
        });
        let hwnd = HWND(hwnd_rx.recv().expect("target hwnd") as *mut _);
        (hwnd, stop_tx, join)
    }

    #[test]
    #[ignore = "creates windows on the desktop; selected by the Windows integration runner"]
    fn attached_wait_retrieves_a_message_posted_to_the_driver_thread() {
        let (target, stop, join) = target_thread();
        let attachment = InputQueueAttachment::attach(target).expect("attach input queues");
        let before = attachment.pumped.get();
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP + 7, WPARAM(0), LPARAM(0)) }
            .expect("post to the driver thread");
        attachment.idle(Duration::ZERO);
        let mut msg = MSG::default();
        let left =
            unsafe { PeekMessageW(&mut msg, None, WM_APP + 7, WM_APP + 7, PM_NOREMOVE) }.as_bool();
        let pumped = attachment.pumped.get() - before;
        drop(attachment);
        let _ = stop.send(());
        join.join().expect("target thread");
        assert!(
            !left,
            "the attached wait left a message queued for the driver thread"
        );
        assert!(pumped >= 1, "the attached wait retrieved nothing");
    }

    #[test]
    #[ignore = "creates windows on the desktop; selected by the Windows integration runner"]
    fn input_handed_to_the_driver_thread_is_never_read_by_the_target() {
        let (target, stop, join) = target_thread();
        let own = popup();
        let attachment = InputQueueAttachment::attach(target).expect("attach input queues");
        assert!(attachment.read_by_target(true));
        unsafe { PostMessageW(Some(own), WM_KEYDOWN, WPARAM(0x41), LPARAM(1)) }
            .expect("post a key to the driver thread's window");
        attachment.idle(Duration::ZERO);
        let misrouted = attachment.misrouted_input.get();
        let read = attachment.read_by_target(true);
        drop(attachment);
        unsafe {
            let _ = DestroyWindow(own);
        }
        let _ = stop.send(());
        join.join().expect("target thread");
        assert_eq!(misrouted, 1);
        assert!(
            !read,
            "a sentinel toggle cannot prove the target read input"
        );
    }
}

#[cfg(test)]
mod thread_activation_tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
    };

    #[test]
    #[ignore = "creates windows on the desktop; selected by the Windows integration runner"]
    fn a_window_its_thread_has_not_activated_is_not_ready_for_attachment() {
        // GetForegroundWindow can already name a window whose thread has not
        // read its activation; only the thread's own active HWND proves it.
        // A fresh, never-activated window is that state.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("create popup")
        };
        let ready = thread_activation_processed(hwnd);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        assert!(!ready);
        assert!(!thread_activation_processed(HWND(std::ptr::null_mut())));
    }
}

/// A target whose UI thread is busy when the driver brings it to the
/// foreground: the activation stays queued until the thread pumps again.
/// Foreground text must still reach it once it does (WPF harness, release
/// gate run 45363565804: 0 of 32 units read while the title bar stayed
/// inactive, then the window activated the moment the driver detached).
#[cfg(test)]
mod busy_target_activation_tests {
    use super::*;
    use std::sync::{mpsc, Mutex};
    use windows::core::w;
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, PostThreadMessageW, RegisterClassW, TranslateMessage, MSG,
        WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        WS_VISIBLE,
    };

    const WM_BUSY: u32 = WM_APP + 1;
    const WM_SELF_ACTIVATE: u32 = WM_APP + 2;
    static TYPED: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    static STALL_AFTER_FIRST_CHAR: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static STALL_ENTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static STALL_RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    struct CharStall;
    impl CharStall {
        fn arm() -> Self {
            use std::sync::atomic::Ordering;
            STALL_ENTERED.store(false, Ordering::SeqCst);
            STALL_RELEASE.store(false, Ordering::SeqCst);
            STALL_AFTER_FIRST_CHAR.store(true, Ordering::SeqCst);
            Self
        }
        fn wait_until_blocked(&self) {
            use std::sync::atomic::Ordering;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !STALL_ENTERED.load(Ordering::SeqCst) {
                assert!(
                    Instant::now() < deadline,
                    "fixture never received the first character"
                );
                sleep(Duration::from_millis(1));
            }
        }
        fn release(&self) {
            use std::sync::atomic::Ordering;
            STALL_RELEASE.store(true, Ordering::SeqCst);
        }
    }
    impl Drop for CharStall {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering;
            STALL_AFTER_FIRST_CHAR.store(false, Ordering::SeqCst);
            STALL_RELEASE.store(true, Ordering::SeqCst);
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CHAR => {
                TYPED.lock().unwrap().push(wparam.0 as u16);
                use std::sync::atomic::Ordering;
                if STALL_AFTER_FIRST_CHAR.swap(false, Ordering::SeqCst) {
                    STALL_ENTERED.store(true, Ordering::SeqCst);
                    while !STALL_RELEASE.load(Ordering::SeqCst) {
                        sleep(Duration::from_millis(1));
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn register() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: GetModuleHandleW(None).expect("module handle").into(),
                lpszClassName: w!("CuaBusyTargetActivation"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0, "register window class");
        });
    }

    /// A top-level window on its own pumping thread. A posted `WM_BUSY`
    /// makes the thread stop pumping until `release` is signalled.
    struct UiThread {
        hwnd: HWND,
        thread_id: u32,
        busy: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl UiThread {
        fn spawn(x: i32) -> Self {
            register();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (busy_tx, busy_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let join = std::thread::spawn(move || unsafe {
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("CuaBusyTargetActivation"),
                    w!("cua busy target"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    x,
                    100,
                    300,
                    200,
                    None,
                    None,
                    Some(GetModuleHandleW(None).expect("module handle").into()),
                    None,
                )
                .expect("create window");
                ready_tx
                    .send((hwnd.0 as isize, GetCurrentThreadId()))
                    .expect("send hwnd");
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.hwnd.0.is_null() && msg.message == WM_SELF_ACTIVATE {
                        // A window its own thread activates while another
                        // app is foreground: active in its queue only.
                        let _ = SetActiveWindow(hwnd);
                        let _ = SetFocus(Some(hwnd));
                        busy_tx.send(()).expect("self-activated");
                        continue;
                    }
                    if msg.hwnd.0.is_null() && msg.message == WM_BUSY {
                        busy_tx.send(()).expect("busy");
                        let _ = release_rx.recv();
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
            let (hwnd, thread_id) = ready_rx.recv().expect("window ready");
            Self {
                hwnd: HWND(hwnd as *mut _),
                thread_id,
                busy: busy_rx,
                release: release_tx,
                join: Some(join),
            }
        }

        fn block(&self) {
            unsafe { PostThreadMessageW(self.thread_id, WM_BUSY, WPARAM(0), LPARAM(0)) }
                .expect("post busy");
            self.busy
                .recv_timeout(Duration::from_secs(5))
                .expect("ui thread blocked");
        }

        fn activate_itself(&self) {
            unsafe { PostThreadMessageW(self.thread_id, WM_SELF_ACTIVATE, WPARAM(0), LPARAM(0)) }
                .expect("post self-activate");
            self.busy
                .recv_timeout(Duration::from_secs(5))
                .expect("ui thread self-activated");
        }

        fn unblock(&self) {
            let _ = self.release.send(());
        }
    }

    impl Drop for UiThread {
        fn drop(&mut self) {
            self.unblock();
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }

    fn state(label: &str, hwnd: HWND) {
        let (active, focus) = gui_thread_active_and_focus(hwnd).unwrap_or_default();
        let mut fg_info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let _ = unsafe { GetGUIThreadInfo(0, &mut fg_info) };
        eprintln!(
            "[busy-target] {label}: fg={:?} target={:?} queue_active={:?} queue_focus={:?} \
             caption_active={} activation_processed={} fg_thread_active={:?} \
             fg_thread_focus={:?}",
            unsafe { GetForegroundWindow() }.0,
            hwnd.0,
            active.0,
            focus.0,
            caption_active(hwnd),
            thread_activation_processed(hwnd),
            fg_info.hwndActive.0,
            fg_info.hwndFocus.0,
        );
    }

    fn bring_forward(hwnd: HWND) {
        let _ = unsafe { crate::input::force_foreground_assisted(hwnd) };
        assert!(
            wait_for_exact_foreground(hwnd, Duration::from_secs(2)),
            "window did not become foreground"
        );
    }

    fn wait_caption(hwnd: HWND, active: bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if caption_active(hwnd) == active {
                return true;
            }
            sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    #[ignore = "activates windows on the interactive desktop; selected by the Windows integration runner"]
    fn foreground_long_mixed_text_is_paced_without_authorizing_focus_return() {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);
        bring_forward(other.hwnd);
        TYPED.lock().unwrap().clear();
        let text = format!("{}二{}", "a".repeat(40), "b".repeat(40));
        let result = send_text_synthesized_paced_after_focus(
            target.hwnd.0 as usize as u64,
            &text,
            0,
            TypingTurn::NotSent,
            || Ok(TypingTurn::NotSent),
        )
        .expect("foreground text insertion");
        assert!(
            !restores_previous(result),
            "a pacing marker cannot confirm input"
        );
        assert_eq!(
            unsafe { GetForegroundWindow() },
            target.hwnd,
            "unconfirmed input must keep the target foreground"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && TYPED.lock().unwrap().len() < text.encode_utf16().count()
        {
            sleep(Duration::from_millis(10));
        }
        let typed = String::from_utf16_lossy(&TYPED.lock().unwrap());
        drop(other);
        drop(target);
        assert_eq!(typed, text);
    }

    #[test]
    #[ignore = "activates windows and sends input on the interactive desktop; selected by the Windows integration runner"]
    fn foreground_text_stops_before_next_batch_when_reader_stalls() {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);
        bring_forward(other.hwnd);
        TYPED.lock().unwrap().clear();
        let stall = CharStall::arm();
        let target_id = target.hwnd.0 as usize as u64;
        let text = "a".repeat(80);
        let sent = text.clone();
        let typing = std::thread::spawn(move || {
            send_text_synthesized_paced_after_focus(
                target_id,
                &sent,
                0,
                TypingTurn::NotSent,
                || Ok(TypingTurn::NotSent),
            )
        });
        stall.wait_until_blocked();
        let result = typing.join().expect("typing thread");
        let error = result
            .expect_err("a stalled reader must stop typing")
            .to_string();
        assert!(error.contains("text_delivery_stalled"), "{error}");
        stall.release();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }
        let typed = TYPED.lock().unwrap().len();
        assert!(
            typed > 0 && typed <= type_pacing::BATCH_UNITS,
            "unexpected typed count {typed}"
        );
        assert!(!restores_previous(TypingTurn::Undrained));
    }

    #[test]
    #[ignore = "activates windows and sends input on the interactive desktop; selected by the Windows integration runner"]
    fn foreground_text_stops_before_next_batch_after_foreground_moves() {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);
        bring_forward(other.hwnd);
        TYPED.lock().unwrap().clear();
        let stall = CharStall::arm();
        let target_id = target.hwnd.0 as usize as u64;
        let typing = std::thread::spawn(move || {
            send_text_synthesized_paced_after_focus(
                target_id,
                &"a".repeat(80),
                0,
                TypingTurn::NotSent,
                || Ok(TypingTurn::NotSent),
            )
        });
        stall.wait_until_blocked();
        bring_forward(other.hwnd);
        let result = typing.join().expect("typing thread");
        let error = result
            .expect_err("a foreground change must stop typing")
            .to_string();
        assert!(error.contains("foreground_lost"), "{error}");
        stall.release();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            sleep(Duration::from_millis(5));
        }
        let typed = TYPED.lock().unwrap().len();
        assert!(
            typed > 0 && typed <= type_pacing::BATCH_UNITS,
            "unexpected typed count {typed}"
        );
    }

    /// A target that activated itself while in the background (as WPF does
    /// on `Window.Activate`) has an active and focused window in its own
    /// queue before the driver brings it forward, so that queue state does
    /// not prove the foreground activation was processed.
    #[test]
    #[ignore = "activates windows on the interactive desktop; ci-rust-windows.yml selects it"]
    fn foreground_text_reaches_a_target_that_activated_itself_in_the_background() {
        self_activated_case(true);
    }

    #[test]
    #[ignore = "activates windows on the interactive desktop; ci-rust-windows.yml selects it"]
    fn foreground_text_reaches_an_idle_target_that_activated_itself_in_the_background() {
        self_activated_case(false);
    }

    fn self_activated_case(busy: bool) {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);
        bring_forward(target.hwnd);
        assert!(wait_caption(target.hwnd, true), "target never activated");
        bring_forward(other.hwnd);
        assert!(wait_caption(target.hwnd, false), "target never deactivated");
        target.activate_itself();
        state("target self-activated in background", target.hwnd);

        if busy {
            target.block();
        }
        TYPED.lock().unwrap().clear();
        let hwnd = target.hwnd.0 as usize as u64;
        let typing = std::thread::spawn(move || send_text_synthesized(hwnd, "abc"));
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            state("self-activated, activation pending", target.hwnd);
            sleep(Duration::from_millis(50));
        }
        target.unblock();
        let result = typing.join().expect("typing thread");
        state("self-activated, after typing", target.hwnd);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && TYPED.lock().unwrap().len() < 3 {
            sleep(Duration::from_millis(10));
        }
        let typed = String::from_utf16_lossy(&TYPED.lock().unwrap());
        eprintln!(
            "[busy-target] self-activated result={:?} typed={typed:?} after {:?}",
            result.as_ref().map_err(|e| e.to_string()),
            started.elapsed()
        );
        drop(other);
        drop(target);
        result.expect("foreground text delivery");
        assert_eq!(typed, "abc");
    }

    /// The window the driver takes the foreground from belongs to a thread
    /// that is busy (not reading its queue) during the switch.
    #[test]
    #[ignore = "activates windows on the interactive desktop; ci-rust-windows.yml selects it"]
    fn foreground_text_reaches_a_target_when_the_previous_foreground_is_busy() {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);
        bring_forward(target.hwnd);
        assert!(wait_caption(target.hwnd, true), "target never activated");
        bring_forward(other.hwnd);
        assert!(wait_caption(target.hwnd, false), "target never deactivated");
        other.block();
        TYPED.lock().unwrap().clear();
        let hwnd = target.hwnd.0 as usize as u64;
        let started = Instant::now();
        let typing = std::thread::spawn(move || send_text_synthesized(hwnd, "abc"));
        while started.elapsed() < Duration::from_millis(300) {
            state("previous busy, typing", target.hwnd);
            sleep(Duration::from_millis(50));
        }
        other.unblock();
        let result = typing.join().expect("typing thread");
        state("previous busy, after typing", target.hwnd);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && TYPED.lock().unwrap().len() < 3 {
            sleep(Duration::from_millis(10));
        }
        let typed = String::from_utf16_lossy(&TYPED.lock().unwrap());
        eprintln!(
            "[busy-target] previous busy result={:?} typed={typed:?} after {:?}",
            result.as_ref().map_err(|e| e.to_string()),
            started.elapsed()
        );
        drop(other);
        drop(target);
        result.expect("foreground text delivery");
        assert_eq!(typed, "abc");
    }

    #[test]
    #[ignore = "activates windows on the interactive desktop; ci-rust-windows.yml selects it"]
    fn foreground_text_reaches_a_target_busy_when_activated() {
        let target = UiThread::spawn(100);
        let other = UiThread::spawn(500);

        // The target has been active before, then lost the foreground: its
        // queue still remembers an active window.
        bring_forward(target.hwnd);
        assert!(wait_caption(target.hwnd, true), "target never activated");
        state("target active", target.hwnd);
        bring_forward(other.hwnd);
        assert!(wait_caption(target.hwnd, false), "target never deactivated");
        state("target in background", target.hwnd);

        // The target's UI thread is busy when the driver activates it.
        target.block();
        TYPED.lock().unwrap().clear();
        let hwnd = target.hwnd.0 as usize as u64;
        let typing = std::thread::spawn(move || send_text_synthesized(hwnd, "abc"));
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            state("activation pending", target.hwnd);
            sleep(Duration::from_millis(50));
        }
        target.unblock();
        let result = typing.join().expect("typing thread");
        state("after typing", target.hwnd);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && TYPED.lock().unwrap().len() < 3 {
            sleep(Duration::from_millis(10));
        }
        let typed = String::from_utf16_lossy(&TYPED.lock().unwrap());
        eprintln!(
            "[busy-target] result={:?} typed={typed:?} after {:?}",
            result.as_ref().map_err(|e| e.to_string()),
            started.elapsed()
        );
        drop(other);
        drop(target);
        result.expect("foreground text delivery");
        assert_eq!(typed, "abc");
    }
}
