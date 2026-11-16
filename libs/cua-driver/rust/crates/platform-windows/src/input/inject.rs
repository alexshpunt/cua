// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Coordinate-routed pen/touch input for background pointer actions.
//!
//! UIA and fixed-HWND PostMessage do not support every application. Synthetic
//! pointer input reaches the visible window at a screen point without an
//! explicit foreground transition or z-order raise. Occluded targets refuse
//! before injection rather than raising another window.
//!
//! `NoActivateGuard` suppresses click activation while injection runs. A target
//! can still activate itself asynchronously, and legacy touch/pen promotion can
//! move the hardware cursor. Injection success does not confirm receiver reads.
//! These routes never race unread input with a timed foreground/cursor restore;
//! callers must report the unobservable read and any background-posture limit.
//!
//! Left/right taps use a synthetic pen. Left drags use touch; right drags use
//! a barrel-held pen. Middle-button injection is unsupported.

use anyhow::{bail, Result};
use core::ffi::c_void;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
};
use windows::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_INFO, POINTER_PEN_INFO, POINTER_TOUCH_INFO,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow,
    LockSetForegroundWindow, SetForegroundWindow, SetWindowLongPtrW, WindowFromPoint, GA_ROOT,
    GWL_EXSTYLE, LSFW_LOCK, LSFW_UNLOCK, PT_PEN, PT_TOUCH, WS_EX_NOACTIVATE,
};

unsafe fn inject_unconfirmed_pointer(
    device: HSYNTHETICPOINTERDEVICE,
    events: &[POINTER_TYPE_INFO],
) -> windows::core::Result<()> {
    let result = unsafe { InjectSyntheticPointerInput(device, events) };
    if result.is_ok() {
        crate::focus_return::note_unconfirmed_input();
    }
    result
}

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
    let attached = cur_tid != 0 && cur_tid != my_tid;
    if attached {
        let _ = AttachThreadInput(my_tid, cur_tid, true);
    }
    let _ = SetForegroundWindow(target);
    if attached {
        let _ = AttachThreadInput(my_tid, cur_tid, false);
    }
    GetForegroundWindow() == target
}

/// Retry an explicit visible foreground transition after a reserved no-name
/// key grants this process the most-recent-input token. The key has no
/// application meaning; the retry count keeps the transition bounded.
pub(crate) unsafe fn force_foreground_assisted(target: HWND) -> (bool, bool) {
    if unsafe { force_foreground_attached(target) } {
        return (true, false);
    }

    const VK_NONAME: u8 = 0xFC;
    unsafe {
        keybd_event(VK_NONAME, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_NONAME, 0, KEYEVENTF_KEYUP, 0);
    }
    for _ in 0..3 {
        if unsafe { force_foreground_attached(target) } {
            return (true, true);
        }
        sleep(Duration::from_millis(25));
    }
    (false, true)
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

/// PEN_FLAG_BARREL (winuser.h) — pen barrel button held == secondary (right)
/// button. `penFlags` is a raw u32 in the bindings, so use the literal.
const PEN_FLAG_BARREL: u32 = 0x00000001;

// (Removed ZorderGuard.) The macOS-aligned contract forbids a `background`
// actuation from raising/restacking the target: coordinate-routed pen/touch
// only lands on the topmost VISIBLE window, so an occluded target is reported
// as `background_unavailable` (see `target_visible_at_point`) and the agent
// escalates to `delivery_mode:"foreground"` instead of the driver raising it.

/// One down→up **pen** tap at screen `(sx, sy)`. When `barrel` is set the pen's
/// barrel button is held for the contact, which the system maps to a secondary
/// (right) click — both for `WM_POINTER`-aware apps (Chromium/WPF/UWP) and via
/// pen→mouse promotion for legacy Win32. A fresh synthetic pen device is
/// created and destroyed per tap (right/middle clicks are rare).
fn pen_taps(sx: i32, sy: i32, barrel: bool, count: usize) -> Result<()> {
    unsafe {
        let dev = CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT)
            .map_err(|e| anyhow::anyhow!("CreateSyntheticPointerDevice(PEN): {e}"))?;
        let pen_flags = if barrel { PEN_FLAG_BARREL } else { 0 };
        let mk = |flags| POINTER_TYPE_INFO {
            r#type: PT_PEN,
            Anonymous: POINTER_TYPE_INFO_0 {
                penInfo: POINTER_PEN_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_PEN,
                        pointerId: 0,
                        pointerFlags: flags,
                        sourceDevice: HANDLE::default(),
                        hwndTarget: HWND::default(),
                        ptPixelLocation: POINT { x: sx, y: sy },
                        ..Default::default()
                    },
                    penFlags: pen_flags,
                    penMask: 0,
                    pressure: 512,
                    rotation: 0,
                    tiltX: 0,
                    tiltY: 0,
                },
            },
        };
        let down = mk(POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT);
        let up = mk(POINTER_FLAG_UP);
        // Reuse the SAME synthetic device for every tap. A double/triple click
        // is two/three down-up cycles from one digitizer; creating a fresh
        // device per tap (the old loop) fails the next
        // CreateSyntheticPointerDevice in quick succession — which is exactly
        // why background double_click on Chromium errored. See #1984.
        let mut result: Result<()> = Ok(());
        let n = count.max(1);
        for i in 0..n {
            let r1 = inject_unconfirmed_pointer(dev, &[down]);
            sleep(Duration::from_millis(25));
            let r2 = inject_unconfirmed_pointer(dev, &[up]);
            if let Err(e) = r1.and(r2) {
                result = Err(anyhow::anyhow!("inject_unconfirmed_pointer(pen): {e}"));
                break;
            }
            if i + 1 < n {
                sleep(Duration::from_millis(70));
            }
        }
        DestroySyntheticPointerDevice(dev);
        result?;
    }
    Ok(())
}

/// Inject a click at screen coordinates `(sx, sy)` without an explicit
/// foreground swap or z-order raise. The visible target is held non-activatable
/// during injection. Pointer promotion and asynchronous self-activation remain
/// observable background-posture limits; no timer authorizes restoration.
///
/// - `left`  → synthetic-pen primary tap (proven path; promoted to a left click
///   for non-pointer-aware apps, and accepted directly by Chromium/WPF/UWP).
/// - `right` → synthetic-pen tap with the barrel button held (secondary click).
/// - `middle`→ unsupported (no clean pointer mapping); returns `Err` so the
///   caller can fall back to its existing routing / structured error.
///
/// We use the same `CreateSyntheticPointerDevice`/`InjectSyntheticPointerInput`
/// path for both buttons — `InjectTouchInput` proved unreliable for left-clicks
/// on Chromium content (returned errors), whereas synthetic-pen injection lands
/// reliably and routes by coordinate with no foreground dependency.
pub fn inject_click_screen(
    target: u64,
    sx: i32,
    sy: i32,
    count: usize,
    button: &str,
) -> Result<()> {
    if target == 0 {
        bail!("inject_click_screen: null target window");
    }
    let target_h = HWND(target as *mut _);
    unsafe {
        if !IsWindow(target_h).as_bool() {
            bail!("inject_click_screen: invalid or stale target HWND");
        }
    }
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(target) {
        // Higher-integrity target: injection into its queue is blocked too.
        bail!(msg);
    }

    let barrel = match button {
        "left" => false,
        "right" => true,
        other => bail!("background injection supports left/right buttons only (got {other:?})"),
    };

    // macOS-aligned contract: a `background` actuation NEVER fronts or raises
    // (mirrors macOS CGEvent-to-pid, which never touches z-order/focus). Synthetic
    // pen/touch is coordinate-routed to the TOP-MOST VISIBLE window at the point,
    // so if the target is occluded there we'd silently click the occluder. We do
    // NOT raise to win the hit-test (that's the foreground rung's job, which the
    // agent opts into). Instead bail — the caller surfaces `background_unavailable`
    // and the agent escalates to `delivery_mode:"foreground"`.
    if unsafe { !target_visible_at_point(target_h, sx, sy) } {
        bail!(
            "background coordinate injection cannot reach this target at ({sx},{sy}) \
             — it is occluded by another window (raising it would break the \
             no-foreground contract). Escalate to delivery_mode:\"foreground\"."
        );
    }
    // Make the target non-activatable without raising it. Injection confirms
    // insertion only; do not reassert another foreground while the pen release
    // may still be unread. Callers must report the unobservable read outcome.
    let _noact = NoActivateGuard::arm(target_h);
    // One synthetic device does all `count` taps (single/double/triple click).
    pen_taps(sx, sy, barrel, count)
}

/// True when `target`'s top-level root is the window actually visible at screen
/// point `(sx, sy)` — i.e. coordinate-routed pen/touch there would land on it
/// rather than an occluder. Used to enforce the macOS-aligned rule that a
/// `background` actuation never raises: when this is false the injection bails
/// and the tool returns `background_unavailable` (escalate to foreground).
/// The foreground SendInput drag uses the same check before its button-down.
pub(crate) unsafe fn target_visible_at_point(target: HWND, sx: i32, sy: i32) -> bool {
    let top = WindowFromPoint(POINT { x: sx, y: sy });
    if top.0.is_null() {
        return false;
    }
    let top_root = GetAncestor(top, GA_ROOT);
    let target_root = GetAncestor(target, GA_ROOT);
    !top_root.0.is_null() && top_root == target_root
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

/// One pen press-drag-release from screen `(sx0,sy0)` to `(sx1,sy1)`, with
/// `steps` interpolated in-contact UPDATE points between the down and the up.
/// A single synthetic pen device is created for the whole stroke. The barrel
/// button is held when `barrel` is set (secondary-button drag).
fn pen_drag(sx0: i32, sy0: i32, sx1: i32, sy1: i32, steps: usize, barrel: bool) -> Result<()> {
    unsafe {
        let dev = CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT)
            .map_err(|e| anyhow::anyhow!("CreateSyntheticPointerDevice(PEN): {e}"))?;
        let pen_flags = if barrel { PEN_FLAG_BARREL } else { 0 };
        let mk = |flags, x: i32, y: i32| POINTER_TYPE_INFO {
            r#type: PT_PEN,
            Anonymous: POINTER_TYPE_INFO_0 {
                penInfo: POINTER_PEN_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_PEN,
                        pointerId: 0,
                        pointerFlags: flags,
                        sourceDevice: HANDLE::default(),
                        hwndTarget: HWND::default(),
                        ptPixelLocation: POINT { x, y },
                        ..Default::default()
                    },
                    penFlags: pen_flags,
                    penMask: 0,
                    pressure: 512,
                    rotation: 0,
                    tiltX: 0,
                    tiltY: 0,
                },
            },
        };
        // Press at the start.
        let down = mk(
            POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
            sx0,
            sy0,
        );
        let mut res = inject_unconfirmed_pointer(dev, &[down]);
        // Interpolated in-contact moves so frameworks that gate drag-tracking on
        // motion (rather than a single down→up) see a continuous stroke.
        let steps = steps.max(1);
        for i in 1..=steps {
            sleep(Duration::from_millis(8));
            let t = i as f64 / steps as f64;
            let x = sx0 + ((sx1 - sx0) as f64 * t).round() as i32;
            let y = sy0 + ((sy1 - sy0) as f64 * t).round() as i32;
            let mv = mk(
                POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
                x,
                y,
            );
            res = res.and(inject_unconfirmed_pointer(dev, &[mv]));
        }
        // Release at the end.
        sleep(Duration::from_millis(8));
        let up = mk(POINTER_FLAG_UP, sx1, sy1);
        res = res.and(inject_unconfirmed_pointer(dev, &[up]));
        DestroySyntheticPointerDevice(dev);
        res.map_err(|e| anyhow::anyhow!("inject_unconfirmed_pointer(pen drag): {e}"))?;
    }
    Ok(())
}

/// A persistent synthetic touch digitizer, kept for the process lifetime so
/// pointer-aware targets can bind to one device. Legacy mouse promotion can
/// still move the hardware cursor; a standing device is not a read witness.
static TOUCH_DEV: Mutex<isize> = Mutex::new(0);

/// One touch press-drag-release via the persistent [`TOUCH_DEV`], serialized
/// across sessions. Pointer-aware targets consume touch directly; legacy
/// promotion may move the hardware cursor, which stays with the target.
fn touch_drag(sx0: i32, sy0: i32, sx1: i32, sy1: i32, steps: usize) -> Result<()> {
    let mut dev_guard = TOUCH_DEV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // Non-pointer-aware windows may promote touch to mouse and move the
        // system cursor. There is no external read witness for that promoted
        // stream, so leave its final posture instead of racing a queued UP with
        // a cursor warp. Callers must report this background-route limitation.
        let dev = if *dev_guard != 0 {
            HSYNTHETICPOINTERDEVICE(*dev_guard as *mut c_void)
        } else {
            let d = CreateSyntheticPointerDevice(PT_TOUCH, 1, POINTER_FEEDBACK_DEFAULT)
                .map_err(|e| anyhow::anyhow!("CreateSyntheticPointerDevice(TOUCH): {e}"))?;
            *dev_guard = d.0 as isize;
            d
        };
        let mk = |flags, x: i32, y: i32| POINTER_TYPE_INFO {
            r#type: PT_TOUCH,
            Anonymous: POINTER_TYPE_INFO_0 {
                touchInfo: POINTER_TOUCH_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_TOUCH,
                        pointerId: 0,
                        pointerFlags: flags,
                        sourceDevice: HANDLE::default(),
                        hwndTarget: HWND::default(),
                        ptPixelLocation: POINT { x, y },
                        ..Default::default()
                    },
                    touchFlags: 0,
                    touchMask: 0,
                    rcContact: RECT {
                        left: x - 2,
                        top: y - 2,
                        right: x + 2,
                        bottom: y + 2,
                    },
                    rcContactRaw: RECT {
                        left: x - 2,
                        top: y - 2,
                        right: x + 2,
                        bottom: y + 2,
                    },
                    orientation: 0,
                    pressure: 512,
                },
            },
        };
        // Keep the existing bounded contact cadence. This timing is not a
        // receiver-read acknowledgment and does not permit pointer restoration.
        let down = mk(
            POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
            sx0,
            sy0,
        );
        let mut res = inject_unconfirmed_pointer(dev, &[down]);
        let steps = steps.clamp(1, 3);
        for i in 1..=steps {
            sleep(Duration::from_millis(2));
            let t = i as f64 / steps as f64;
            let x = sx0 + ((sx1 - sx0) as f64 * t).round() as i32;
            let y = sy0 + ((sy1 - sy0) as f64 * t).round() as i32;
            let mv = mk(
                POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
                x,
                y,
            );
            res = res.and(inject_unconfirmed_pointer(dev, &[mv]));
        }
        sleep(Duration::from_millis(2));
        let up = mk(POINTER_FLAG_UP, sx1, sy1);
        res = res.and(inject_unconfirmed_pointer(dev, &[up]));
        // Keep any promoted pointer with the target. Injection is not evidence
        // that the receiver has retrieved the contact or promoted mouse UP.
        // device intentionally NOT destroyed — see TOUCH_DEV.
        res.map_err(|e| anyhow::anyhow!("inject_unconfirmed_pointer(touch drag): {e}"))?;
    }
    Ok(())
}

/// Inject a press-drag-release at screen coordinates without an explicit
/// foreground swap or z-order raise. Left uses touch and right uses a barrel-held
/// pen. The target is held non-activatable during insertion. Legacy promotion
/// can move the cursor, and asynchronous activation can outlive the call;
/// callers must report those limits without a timed restoration.
pub fn inject_drag_screen(
    target: u64,
    sx0: i32,
    sy0: i32,
    sx1: i32,
    sy1: i32,
    steps: usize,
    button: &str,
) -> Result<()> {
    if target == 0 {
        bail!("inject_drag_screen: null target window");
    }
    let target_h = HWND(target as *mut _);
    unsafe {
        if !IsWindow(target_h).as_bool() {
            bail!("inject_drag_screen: invalid or stale target HWND");
        }
    }
    if let Some(msg) = crate::input::post_message_blocked_by_uipi(target) {
        bail!(msg);
    }
    let barrel = match button {
        "left" => false,
        "right" => true,
        other => bail!("background injection supports left/right buttons only (got {other:?})"),
    };
    // macOS-aligned contract: a `background` drag NEVER fronts/raises. WPF's
    // Wisp stylus stack only PROCESSES injected input while the window is the
    // ACTIVE foreground (verified by RE) — and background is not allowed to
    // force that — so a WPF drag is simply undeliverable in background. Bail so
    // the tool returns background_unavailable (escalate to foreground).
    if crate::input::delivery::is_wpf_target_window(target) {
        bail!(
            "background drag is not deliverable to a WPF target — its Wisp stylus \
             stack only processes injected input while the window is foreground, \
             which background must not force. Escalate to delivery_mode:\"foreground\"."
        );
    }
    // Coordinate-routed touch/pen lands on the topmost VISIBLE window at the
    // start point. If the target is occluded there, bail rather than raise it.
    if unsafe { !target_visible_at_point(target_h, sx0, sy0) } {
        bail!(
            "background drag cannot reach this target at the start point ({sx0},{sy0}) \
             — it is occluded by another window. Escalate to delivery_mode:\"foreground\"."
        );
    }
    // Left drag → touch contact (coordinate-routed). Right/barrel drag has no
    // touch equivalent, so fall back to a pen (rare).
    let stroke = |()| {
        if barrel {
            pen_drag(sx0, sy0, sx1, sy1, steps, true)
        } else {
            touch_drag(sx0, sy0, sx1, sy1, steps)
        }
    };
    // Prevent click activation without raising the target. Do not reassert a
    // different foreground while injected or promoted input may still be unread.
    let _noact = NoActivateGuard::arm(target_h);
    stroke(())
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
