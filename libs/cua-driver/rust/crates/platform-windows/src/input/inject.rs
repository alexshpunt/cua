// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Foreground guards and background pointer capability boundaries.
//!
//! Semantic UIA and fixed-HWND PostMessage routes remain available. Synthetic
//! background pen/touch can self-activate the target or promote to the human's
//! cursor, with no supported read witness permitting safe restoration. Refuse
//! those routes before mutating the desktop or inserting any input.

use anyhow::{bail, Result};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    LockSetForegroundWindow, SetForegroundWindow, SetWindowLongPtrW, GA_ROOT, GWL_EXSTYLE,
    LSFW_LOCK, LSFW_UNLOCK, WS_EX_NOACTIVATE,
};

#[derive(Default)]
struct ForegroundLockState {
    holders: usize,
    locked: bool,
}

static FOREGROUND_LOCK_STATE: Mutex<ForegroundLockState> = Mutex::new(ForegroundLockState {
    holders: 0,
    locked: false,
});

/// Prevent other processes from taking the foreground during a background
/// launch. Windows automatically clears this lock on genuine user input; Drop
/// still balances the documented unlock call and overlapping driver launches.
pub struct ForegroundLockGuard {
    held: bool,
}

impl ForegroundLockGuard {
    pub fn acquire() -> Self {
        let mut state = FOREGROUND_LOCK_STATE.lock().unwrap();
        if state.holders == 0 {
            state.locked = unsafe { LockSetForegroundWindow(LSFW_LOCK) }.is_ok();
            if state.locked {
                tracing::debug!(target: "launch_app.focus_lock", "locked foreground changes during background launch");
            } else {
                tracing::warn!(target: "launch_app.focus_lock", "could not lock foreground changes during background launch");
            }
        }
        if state.locked {
            state.holders += 1;
        }
        Self { held: state.locked }
    }

    pub fn acquired(&self) -> bool {
        self.held
    }
}

impl Drop for ForegroundLockGuard {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut state = FOREGROUND_LOCK_STATE.lock().unwrap();
        state.holders = state.holders.saturating_sub(1);
        if state.holders == 0 {
            let _ = unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
            state.locked = false;
            tracing::debug!(target: "launch_app.focus_lock", "unlocked foreground changes after background launch");
        }
    }
}

/// Bring `target` to the foreground using the AttachThreadInput trick, which
/// inherits the current foreground thread's FG-lock token so the swap is
/// honored even on a foreground-locked session without UIAccess (mirrors the
/// `bring_to_front` tool). Single attach, no retry loop — bounded. Returns
/// whether `target` actually became foreground.
pub(crate) unsafe fn force_foreground_attached(target: HWND) -> bool {
    let cur = GetForegroundWindow();
    if cur == target {
        return true;
    }
    let my_tid = GetCurrentThreadId();
    let mut pid = 0u32;
    let cur_tid = GetWindowThreadProcessId(cur, Some(&mut pid));
    // Attaching resets key state. An explicit activation may proceed, but
    // must not reset the receiver state of an earlier unconfirmed prefix.
    let attached =
        if cur_tid != 0 && cur_tid != my_tid && !crate::focus_return::has_unconfirmed_input() {
            // A fresh blocking worker must establish its message queue first.
            // This runs before attachment, so it cannot inspect the target's input.
            super::keyboard::service_own_messages();
            let attached = AttachThreadInput(my_tid, cur_tid, true).as_bool();
            if !attached {
                tracing::debug!(
                    own_thread = my_tid,
                    foreground_thread = cur_tid,
                    error = %windows::core::Error::from_win32(),
                    "foreground queue attachment failed"
                );
            }
            attached
        } else {
            false
        };
    let requested = SetForegroundWindow(target).as_bool();
    if attached {
        let _ = AttachThreadInput(my_tid, cur_tid, false);
    }
    // Across input queues, successful activation is asynchronous. Wait for its
    // nudge before rejecting it and inserting an unnecessary unlock key.
    // This pre-input readiness check never acknowledges inserted input.
    let ready = requested && super::keyboard::activation_ready(target);
    let actual = GetForegroundWindow();
    tracing::debug!(
        attached,
        requested,
        ready,
        target = ?target,
        actual = ?actual,
        "foreground activation observation"
    );
    if actual == target {
        crate::desktop_activity::note_shared();
    }
    actual == target
}

/// Retry an explicit visible foreground transition after this process becomes
/// the source of the most recent input, which Windows requires before it lets a
/// process change the foreground. A null mouse input does that without any
/// message reaching a window; the no-name key pulse is the fallback. The retry
/// count keeps the transition bounded.
pub(crate) unsafe fn force_foreground_assisted(target: HWND) -> (bool, bool) {
    if unsafe { force_foreground_attached(target) } {
        return (true, false);
    }
    for unlock in [null_mouse_input as fn(), unlock_pulse] {
        unlock();
        for _ in 0..3 {
            if unsafe { force_foreground_attached(target) } {
                return (true, true);
            }
            sleep(Duration::from_millis(25));
        }
    }
    (false, true)
}

/// Insert a mouse input without movement, buttons or wheel. It makes this process the source of
/// the most recent input, but produces no window message, so nothing is queued for a window to
/// read and focus return needs no read evidence for it.
fn null_mouse_input() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT};
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT::default(),
        },
    };
    let sent =
        unsafe { crate::input::send_desktop_input(&[input], std::mem::size_of::<INPUT>() as i32) };
    tracing::debug!(sent, "foreground unlock: null mouse input");
}

/// The reserved no-name key of the unlock pulse.
const VK_NONAME: u16 = 0xFC;

/// Insert the fallback unlock pulse, a press and a release of the no-name key. The window in
/// front, usually the human's, receives it, and nothing witnesses that it read the key, so the
/// pulse is unconfirmed input.
fn unlock_pulse() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(VK_NONAME),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    crate::focus_return::note_unconfirmed_input();
    let pulse = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    let sent =
        unsafe { crate::input::send_desktop_input(&pulse, std::mem::size_of::<INPUT>() as i32) };
    if sent == 1 {
        // Only the press went in; do not leave the key down.
        let _ = super::keyboard::release_inserted_keys(&pulse, 1);
    }
    tracing::debug!(sent, "foreground unlock: no-name key pulse");
}

/// RAII guard that makes a specific target window **unable to become the
/// foreground/active window** for the duration of an actuation, by adding the
/// `WS_EX_NOACTIVATE` extended style to its top-level window.
///
/// Why this and not a global foreground-lock: our own injected/posted input
/// (or a UIA-Invoke) legitimizes the target's foreground claim, so even a
/// maxed `SPI_*FOREGROUNDLOCKTIMEOUT` won't stop the steal. `WS_EX_NOACTIVATE`
/// is categorical — Windows refuses to activate the window *at all* (clicks,
/// `SetForegroundWindow(self)` from WPF/XAML/Tauri handlers, mouse-activate) —
/// while the window still RECEIVES the click/key. It is per-window (no session
/// side effects) and reversed on drop. Covers the self-activation that the
/// EnableWindow/UWP bypass cannot (WPF `UIElement.Focus()`→SetForegroundWindow).
///
/// Not armed on a window parked on another Task View desktop (Win+Ctrl+D):
/// toggling the style re-registers its taskbar button, and the shell files it
/// under the desktop the user is viewing, so it lands there still cloaked
/// (#5711).
pub struct NoActivateGuard {
    // Store the handle as an integer so the guard is `Send` and can be held
    // across `.await` in the async tools.
    root_addr: isize,
    applied: bool,
}

impl NoActivateGuard {
    /// Arm on the top-level (GA_ROOT) ancestor of `hwnd`.
    pub fn arm(hwnd: HWND) -> Self {
        unsafe {
            let root = {
                let r = GetAncestor(hwnd, GA_ROOT);
                if r.0.is_null() {
                    hwnd
                } else {
                    r
                }
            };
            let prev = GetWindowLongPtrW(root, GWL_EXSTYLE);
            let want = WS_EX_NOACTIVATE.0 as isize;
            // Apply WS_EX_NOACTIVATE if not already set (prev can be 0, so don't
            // gate on it — we just need to check the bit and set it if absent).
            let applied = (prev & want) == 0 && on_current_task_view_desktop(root) && {
                SetWindowLongPtrW(root, GWL_EXSTYLE, prev | want);
                // Confirm it took (cross-process SetWindowLongPtr can be denied
                // by UIPI on higher-integrity targets).
                (GetWindowLongPtrW(root, GWL_EXSTYLE) & want) != 0
            };
            Self {
                root_addr: root.0 as isize,
                applied,
            }
        }
    }
}

impl Drop for NoActivateGuard {
    fn drop(&mut self) {
        if self.applied {
            unsafe {
                let root = HWND(self.root_addr as *mut _);
                let current = GetWindowLongPtrW(root, GWL_EXSTYLE);
                let noactivate = WS_EX_NOACTIVATE.0 as isize;
                // Clear only the bit this guard added. Restoring the full
                // captured value can clobber unrelated style changes the app
                // made while the background action was in flight.
                SetWindowLongPtrW(root, GWL_EXSTYLE, current & !noactivate);
            }
        }
    }
}

/// True when top-level `root` belongs to the Task View desktop the user is
/// viewing. Fails open (`true`) when the shell can't answer, e.g. no Explorer
/// in the session, so `NoActivateGuard` keeps its usual behavior there.
fn on_current_task_view_desktop(root: HWND) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let manager: IVirtualDesktopManager =
            match CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) {
                Ok(manager) => manager,
                Err(_) => return true,
            };
        manager
            .IsWindowOnCurrentVirtualDesktop(root)
            .map_or(true, |on| on.as_bool())
    }
}

/// Refuse legacy background synthetic clicks before any desktop mutation.
/// Use a semantic UIA route, a supported fixed-HWND post, or explicit foreground input.
pub fn inject_click_screen(
    _target: u64,
    _sx: i32,
    _sy: i32,
    _count: usize,
    _button: &str,
) -> Result<()> {
    bail!(super::delivery::BACKGROUND_POINTER_UNAVAILABLE)
}

/// True when screen point `(x, y)` lies within `hwnd`'s window rectangle.
///
/// Guards **element_index** clicks: an element's cached center can fall outside
/// its own window when the element is scrolled out of a ScrollViewer or pushed
/// off-screen (e.g. a tall form on a small display). Tapping the raw coordinate
/// then lands on whatever is actually there — the taskbar, the desktop, another
/// window — instead of the intended element. The click tool turns a `false`
/// here into a clear error rather than clicking the wrong target. Returns `true`
/// (fail-open) when the rect can't be read, so legitimate clicks are never
/// blocked by a transient query failure.
pub fn point_in_window_bounds(hwnd: u64, x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    if hwnd == 0 {
        return true;
    }
    let mut r = RECT::default();
    let ok = unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut r).is_ok() };
    if !ok {
        return true;
    }
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn point_in_rect(x: i32, y: i32, left: i32, top: i32, width: i32, height: i32) -> bool {
    if width <= 0 || height <= 0 {
        return false;
    }
    let (x, y, left, top, width, height) = (
        i64::from(x),
        i64::from(y),
        i64::from(left),
        i64::from(top),
        i64::from(width),
        i64::from(height),
    );
    x >= left && x < left + width && y >= top && y < top + height
}

/// True when `(x, y)` belongs to the live Windows virtual desktop.
///
/// Unlike a fixed negative-coordinate threshold, this admits every layout the
/// OS actually reports. It also rejects the iconic `(-32000, -32000)` region
/// when a transient `IsIconic` query races with UIA/GetWindowRect state.
pub fn point_on_virtual_desktop(x: i32, y: i32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        point_in_rect(
            x,
            y,
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// True when `hwnd` is minimized (iconic).
///
/// Authoritative counterpart to [`is_iconic_sentinel_point`]: the capture path
/// already refuses iconic windows outright (see `capture.rs`), and the input
/// path needs the same signal so an element action can fail loudly instead of
/// posting into the off-screen iconic region.
pub fn window_is_iconic(hwnd: u64) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsIconic;
    if hwnd == 0 {
        return false;
    }
    unsafe { IsIconic(HWND(hwnd as *mut _)).as_bool() }
}

#[cfg(test)]
mod task_view_desktop_tests {
    use super::*;
    use std::time::Instant;
    use windows::core::{w, GUID};
    use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
        RegisterClassW, ShowWindow, UnregisterClassW, MSG, PM_REMOVE, SW_SHOWNOACTIVATE,
        WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    /// How long Explorer gets to act on a style change before we look.
    const SETTLE: Duration = Duration::from_millis(1500);
    /// Roughly how long a posted click burst keeps the guard armed.
    const HOLD: Duration = Duration::from_millis(100);

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Pump this thread's messages until `done` holds or `timeout` passes.
    fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            sleep(Duration::from_millis(20));
        }
    }

    /// Task View desktop ids as Explorer persists them (16-byte GUIDs).
    fn task_view_desktop_ids() -> Vec<GUID> {
        let mut bytes = vec![0u8; 16 * 64];
        let mut len = bytes.len() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops"),
                w!("VirtualDesktopIDs"),
                RRF_RT_REG_BINARY,
                None,
                Some(bytes.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        bytes[..len as usize]
            .chunks_exact(16)
            .map(|b| {
                GUID::from_values(
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    u16::from_le_bytes([b[4], b[5]]),
                    u16::from_le_bytes([b[6], b[7]]),
                    [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
                )
            })
            .collect()
    }

    /// #5711. No public API creates a Task View desktop, so make a second one
    /// first (Win+Ctrl+D, then Win+Ctrl+Left to return):
    /// `cargo test -p platform-windows --lib task_view_desktop -- --ignored`
    #[test]
    #[ignore = "needs an interactive session with a second Task View desktop; shows a window"]
    fn guard_leaves_a_window_on_another_task_view_desktop_where_it_is() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let manager: IVirtualDesktopManager =
                CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)
                    .expect("virtual desktop manager");
            let desktop_of = |hwnd: HWND| manager.GetWindowDesktopId(hwnd).unwrap_or_default();
            let instance = GetModuleHandleW(None).expect("module handle");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("CuaNoActivateTaskViewDesktop"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0, "register window class");
            // A plain unowned top-level window has a taskbar button that
            // WS_EX_NOACTIVATE takes away. WS_EX_APPWINDOW would pin it and hide
            // the bug.
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class.lpszClassName,
                w!("cua NoActivateGuard Task View desktop test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                320,
                200,
                None,
                None,
                instance,
                None,
            )
            .expect("create window");
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let registered = pump_until(Duration::from_secs(3), || {
                desktop_of(hwnd) != GUID::zeroed()
            });
            assert!(
                registered,
                "the shell assigned the window a Task View desktop"
            );
            let current = desktop_of(hwnd);
            let other = task_view_desktop_ids()
                .into_iter()
                .find(|id| *id != current)
                .expect("a second Task View desktop (Win+Ctrl+D, then Win+Ctrl+Left)");

            manager
                .MoveWindowToDesktop(hwnd, &other)
                .expect("move the test's own window to the other desktop");
            assert!(
                pump_until(Duration::from_secs(3), || desktop_of(hwnd) == other),
                "the window reached the other desktop"
            );
            // Explorer reports the new desktop before it finishes the move; a
            // style change in that window doesn't reproduce the bug.
            pump_until(SETTLE, || false);
            // Hold the guard across a pumped interval, as an actuation does, so
            // Explorer observes the style while it is set.
            let guard = NoActivateGuard::arm(hwnd);
            pump_until(HOLD, || false);
            drop(guard);
            pump_until(SETTLE, || false);
            let landed = desktop_of(hwnd);

            // Back on the user's desktop, the guard still applies.
            manager
                .MoveWindowToDesktop(hwnd, &current)
                .expect("move the window back to the user's desktop");
            assert!(
                pump_until(Duration::from_secs(3), || desktop_of(hwnd) == current),
                "the window returned to the user's desktop"
            );
            let guard = NoActivateGuard::arm(hwnd);
            let armed = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_NOACTIVATE.0 as isize;
            drop(guard);

            DestroyWindow(hwnd).expect("destroy window");
            UnregisterClassW(class.lpszClassName, instance).expect("unregister class");
            assert_eq!(landed, other, "the guard took the window off its desktop");
            assert_ne!(armed, 0, "on the user's desktop the guard still applies");
        }
    }
}

/// Refuse legacy background synthetic drags before any desktop mutation.
/// Foreground timeline pen input is a separate, explicit delivery route.
pub fn inject_drag_screen(
    _target: u64,
    _sx0: i32,
    _sy0: i32,
    _sx1: i32,
    _sy1: i32,
    _steps: usize,
    _button: &str,
) -> Result<()> {
    bail!(super::delivery::BACKGROUND_POINTER_UNAVAILABLE)
}

#[cfg(test)]
mod iconic_sentinel_tests {
    use super::*;

    /// The exact rect Win32 hands back for a minimized window, as documented on
    /// the capture-path guard in `capture.rs`.
    const ICONIC_RECT: (i32, i32, i32, i32) = (-32000, -32000, -31840, -31972);

    #[test]
    fn virtual_desktop_membership_has_no_magic_negative_ceiling() {
        assert!(point_in_rect(-31920, 50, -40000, 0, 50000, 2000));
    }

    #[test]
    fn iconic_center_is_outside_an_ordinary_virtual_desktop() {
        let (left, top, right, bottom) = ICONIC_RECT;
        let (cx, cy) = ((left + right) / 2, (top + bottom) / 2);
        assert!(!point_in_rect(cx, cy, -2560, -1080, 6400, 3240));
    }

    #[test]
    fn iconic_value_on_either_axis_is_outside_the_desktop() {
        assert!(!point_in_rect(640, -32000, -2560, -1080, 6400, 3240));
        assert!(!point_in_rect(-32000, 480, -2560, -1080, 6400, 3240));
    }

    #[test]
    fn real_negative_origin_monitor_points_remain_valid() {
        for (x, y) in [
            (-1920, 0),
            (-1795, 383),
            (-2560, 0),
            (0, -1080),
            (-1920, -1080),
            (0, 0),
            (1920, 1080),
        ] {
            assert!(point_in_rect(x, y, -2560, -1080, 6400, 3240));
        }
    }

    #[test]
    fn virtual_desktop_edges_and_invalid_extents_fail_closed() {
        assert!(point_in_rect(-1795, 383, -2560, -1080, 6400, 3240));
        assert!(!point_in_rect(-2561, 383, -2560, -1080, 6400, 3240));
        assert!(!point_in_rect(3840, 0, -2560, -1080, 6400, 3240));
        assert!(!point_in_rect(0, 0, 0, 0, 0, 1080));
        assert!(!point_in_rect(0, 0, 0, 0, 1920, -1));
    }
}

// (Removed inject_key_cloaked / inject_text_cloaked.) The macOS-aligned
// contract forbids a `background` actuation from grabbing focus — even a
// *cloaked* (hidden) one. Keyboard/text that the target's input stack would
// drop in the background (VCL/classic-Win32 accelerators, Chromium key-combos,
// WPF/terminal text) is now reported as `background_unavailable`; the agent
// escalates to `delivery_mode:"foreground"`, which uses the explicit
// SetForegroundWindow path (send_key_synthesized / send_text_synthesized).
