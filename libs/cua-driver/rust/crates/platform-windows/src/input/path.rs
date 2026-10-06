//! Foreground Windows held paths, using the same SendInput backend as clicks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE,
    MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, IsWindowVisible, SetCursorPos,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::drag_path::{self, Backend, Path, Point, Report};

type MapPoint = fn(u64, i32, i32) -> (i32, i32);

/// The worker keeps this flag and input admission alive until its release finishes.
pub(crate) struct CancelOnDrop(pub Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct NativePath {
    hwnd: u64,
    pid: u32,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    pressed_at: Instant,
    held: bool,
    previous_foreground: u64,
    previous_cursor: POINT,
    last_cursor: Option<(i32, i32)>,
    cursor_key: String,
    held_keys: Vec<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY>,
    hold_started: Instant,
    hold_duration_ms: Option<u64>,
}

impl NativePath {
    fn send(&self, input: INPUT) -> Result<(), String> {
        let inserted = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if inserted == 1 {
            Ok(())
        } else {
            Err("input_failed: SendInput inserted 0/1 events".into())
        }
    }

    fn button_input(flag: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flag,
                    ..Default::default()
                },
            },
        }
    }

    fn screen_point(&self, point: Point) -> Result<(i32, i32), String> {
        let origin = (self.map)(self.hwnd, 0, 0);
        let screen = (
            origin
                .0
                .checked_add(point.x)
                .ok_or("path_point_outside_window: X overflow")?,
            origin
                .1
                .checked_add(point.y)
                .ok_or("path_point_outside_window: Y overflow")?,
        );
        if !crate::input::point_in_window_bounds(self.hwnd, screen.0, screen.1)
            || !crate::input::point_on_virtual_desktop(screen.0, screen.1)
        {
            return Err(
                "path_point_outside_window: point is outside the current target/display".into(),
            );
        }
        Ok(screen)
    }
}

impl Backend for NativePath {
    fn check_target(&mut self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("cancelled".into());
        }
        if crate::win32::window_owner_pid(self.hwnd) != Some(self.pid)
            || !unsafe { IsWindowVisible(HWND(self.hwnd as *mut _)) }.as_bool()
            || crate::input::window_is_iconic(self.hwnd)
        {
            return Err("target_lost: exact window is gone, hidden or minimized".into());
        }
        if unsafe { GetForegroundWindow() }.0 as usize as u64 != self.hwnd {
            return Err("foreground_lost: stopped before another path event".into());
        }
        Ok(())
    }

    fn move_to(&mut self, point: Point) -> Result<(), String> {
        if self.hold_duration_ms.is_some_and(|duration| {
            self.hold_started.elapsed() > Duration::from_millis(duration + 50)
        }) && !self.held_keys.is_empty()
        {
            return Err("hold_duration_exceeded: stopped before another pointer event".into());
        }
        let (x, y) = self.screen_point(point)?;
        let (dx, dy) = unsafe {
            crate::virtualdesk::to_virtualdesk_absolute(
                x,
                y,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
            )
        };
        self.send(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    dwFlags: MOUSEEVENTF_MOVE
                        | MOUSEEVENTF_ABSOLUTE
                        | MOUSEEVENTF_VIRTUALDESK
                        | MOUSEEVENTF_MOVE_NOCOALESCE,
                    ..Default::default()
                },
            },
        })?;
        self.last_cursor = Some((x, y));
        crate::overlay::send_command(
            self.cursor_key.clone(),
            cursor_overlay::OverlayCommand::SnapTo {
                x: f64::from(x),
                y: f64::from(y),
                heading_radians: None,
            },
        );
        Ok(())
    }

    fn press(&mut self) -> Result<(), String> {
        self.send(Self::button_input(MOUSEEVENTF_LEFTDOWN))?;
        self.held = true;
        self.pressed_at = Instant::now();
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        if self.held {
            self.send(Self::button_input(MOUSEEVENTF_LEFTUP))
                .map_err(|error| format!("release_failed: {error}"))?;
            self.held = false;
        }
        Ok(())
    }

    fn wait_until(&mut self, elapsed_ms: u64) -> Result<(), String> {
        let deadline = Duration::from_millis(elapsed_ms);
        while self.pressed_at.elapsed() < deadline {
            self.check_target()?;
            sleep(
                (deadline - self.pressed_at.elapsed().min(deadline)).min(Duration::from_millis(10)),
            );
        }
        Ok(())
    }
}

impl Drop for NativePath {
    fn drop(&mut self) {
        // The pure state machine already released on handled failures. This also
        // covers a panic after native insertion but before the backend returned.
        let _ = self.release();
        let _ = crate::key_hold::Backend::release_keys(self);
        if self.held || !self.held_keys.is_empty() {
            return;
        }
        // Let the target consume its queued button-up before any restoration.
        sleep(Duration::from_millis(120));
        unsafe {
            let mut current = POINT::default();
            if GetCursorPos(&mut current).is_ok()
                && self.last_cursor == Some((current.x, current.y))
            {
                let _ = SetCursorPos(self.previous_cursor.x, self.previous_cursor.y);
            }
            if GetForegroundWindow().0 as usize as u64 == self.hwnd
                && self.previous_foreground != self.hwnd
                && crate::win32::window_owner_pid(self.previous_foreground).is_some()
            {
                let _ = crate::input::force_foreground_attached(HWND(
                    self.previous_foreground as *mut _,
                ));
            }
        }
    }
}

/// Deliver a validated path with one left press/release. No background fallback.
pub(crate) fn send_path(
    hwnd: u64,
    pid: u32,
    path: &Path,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    cursor_key: String,
) -> Result<Report, String> {
    let mut native = prepare(hwnd, pid, cancelled, map, cursor_key, &[], &[path])?;
    Ok(drag_path::execute(path, &mut native))
}

/// One native admission owns keyboard state and all pointer work until release.
pub(crate) fn send_hold(
    hwnd: u64,
    pid: u32,
    hold: &crate::key_hold::Hold,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    cursor_key: String,
) -> Result<crate::key_hold::Report, String> {
    let paths = hold.actions.iter().collect::<Vec<_>>();
    let mut native = prepare(hwnd, pid, cancelled, map, cursor_key, &hold.keys, &paths)?;
    native.hold_duration_ms = Some(hold.duration_ms);
    native.hold_started = Instant::now();
    let mut report = crate::key_hold::execute(hold, &mut native);
    report.held_ms = native.hold_started.elapsed().as_millis() as u64;
    Ok(report)
}

fn prepare(
    hwnd: u64,
    pid: u32,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    cursor_key: String,
    keys: &[String],
    paths: &[&Path],
) -> Result<NativePath, String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    if cancelled.load(Ordering::Acquire) {
        return Err("cancelled".into());
    }
    if crate::win32::window_owner_pid(hwnd) != Some(pid)
        || crate::input::window_is_iconic(hwnd)
        || !unsafe { IsWindowVisible(HWND(hwnd as *mut _)) }.as_bool()
    {
        return Err("target_lost: exact window is not visible".into());
    }
    if let Some(error) = crate::input::post_message_blocked_by_uipi(hwnd) {
        return Err(format!("input_integrity_denied: {error}"));
    }
    let mut busy = vec![VK_LBUTTON, VK_RBUTTON, VK_MBUTTON];
    if !keys.is_empty() {
        busy.extend([VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]);
        for key in keys {
            busy.push(crate::input::keyboard::key_name_to_vk(key).map_err(|e| e.to_string())?);
        }
    }
    for key in busy {
        if unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000 != 0 {
            return Err(
                "input_busy: a requested key, modifier or mouse button is already held".into(),
            );
        }
    }
    let mut previous_cursor = POINT::default();
    unsafe { GetCursorPos(&mut previous_cursor) }
        .map_err(|error| format!("cursor_unavailable: {error}"))?;
    let native = NativePath {
        hwnd,
        pid,
        cancelled,
        map,
        pressed_at: Instant::now(),
        held: false,
        previous_foreground: unsafe { GetForegroundWindow() }.0 as usize as u64,
        previous_cursor,
        last_cursor: None,
        cursor_key,
        held_keys: Vec::with_capacity(keys.len()),
        hold_started: Instant::now(),
        hold_duration_ms: None,
    };
    for path in paths {
        for point in &path.points {
            native.screen_point(*point)?;
        }
    }
    if !unsafe { crate::input::force_foreground_assisted(HWND(hwnd as *mut _)) }.0 {
        return Err("foreground_unavailable: exact window did not become foreground".into());
    }
    Ok(native)
}

impl crate::key_hold::Backend for NativePath {
    fn press_key(&mut self, key: &str) -> Result<(), String> {
        let vk = crate::input::keyboard::key_name_to_vk(key).map_err(|error| error.to_string())?;
        if unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16 & 0x8000 != 0 {
            return Err("input_busy: requested key became held before insertion".into());
        }
        if self.held_keys.is_empty() {
            self.hold_started = Instant::now();
        }
        self.send(crate::input::keyboard::key_input(vk, false))?;
        self.held_keys.push(vk);
        Ok(())
    }
    fn release_keys(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for i in (0..self.held_keys.len()).rev() {
            match self.send(crate::input::keyboard::key_input(self.held_keys[i], true)) {
                Ok(()) => {
                    self.held_keys.remove(i);
                }
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("key_release_failed: {}", errors.join("; ")))
        }
    }
    fn wait_hold(&mut self, duration_ms: u64) -> Result<(), String> {
        let deadline = Duration::from_millis(duration_ms);
        while self.hold_started.elapsed() < deadline {
            self.check_target()?;
            sleep(
                (deadline - self.hold_started.elapsed().min(deadline))
                    .min(Duration::from_millis(10)),
            );
        }
        Ok(())
    }
}
