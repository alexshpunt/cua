//! Native Windows backend for `input_timeline`: SendInput for keys, buttons,
//! motion and wheel, and one synthetic pen device for pen contacts.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND, POINT};
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_FLAGS, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT,
    POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_INFO, POINTER_PEN_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, IsWindowVisible, PT_PEN, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::timeline::{self, Backend, Button, Event, Pen, Report, Timed};

type MapPoint = fn(u64, i32, i32) -> (i32, i32);

const PEN_FLAG_BARREL: u32 = 0x1;
const PEN_FLAG_ERASER: u32 = 0x4;
const PEN_MASK_PRESSURE: u32 = 0x1;
const PEN_MASK_TILT_X: u32 = 0x4;
const PEN_MASK_TILT_Y: u32 = 0x8;
const XBUTTON1: u32 = 0x1;
const XBUTTON2: u32 = 0x2;

struct Native {
    hwnd: u64,
    pid: u32,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    started: Instant,
    pen: Option<HSYNTHETICPOINTERDEVICE>,
    /// The target and its owner chain; the target's own popups and flyouts may hold the foreground.
    foreground: crate::win32::ForegroundTarget,
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn button_flags(button: Button, down: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    match (button, down) {
        (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (Button::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (Button::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (Button::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2),
    }
}

impl Native {
    fn insert(&self, input: INPUT) -> Result<(), String> {
        let inserted = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if inserted == 1 {
            Ok(())
        } else {
            Err("input_failed: SendInput inserted 0/1 events".into())
        }
    }

    fn screen(&self, x: i32, y: i32) -> Result<(i32, i32), String> {
        let origin = (self.map)(self.hwnd, 0, 0);
        let point = (
            origin
                .0
                .checked_add(x)
                .ok_or("timeline_point_outside_window")?,
            origin
                .1
                .checked_add(y)
                .ok_or("timeline_point_outside_window")?,
        );
        if !crate::input::point_in_window_bounds(self.hwnd, point.0, point.1)
            || !crate::input::point_on_virtual_desktop(point.0, point.1)
        {
            return Err(
                "timeline_point_outside_window: point is outside the current target window".into(),
            );
        }
        Ok(point)
    }

    fn move_absolute(&self, x: i32, y: i32) -> Result<(), String> {
        let (sx, sy) = self.screen(x, y)?;
        let (dx, dy) = unsafe {
            crate::virtualdesk::to_virtualdesk_absolute(
                sx,
                sy,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
            )
        };
        self.insert(mouse(
            MOUSEEVENTF_MOVE
                | MOUSEEVENTF_ABSOLUTE
                | MOUSEEVENTF_VIRTUALDESK
                | MOUSEEVENTF_MOVE_NOCOALESCE,
            dx,
            dy,
            0,
        ))
    }

    fn pen_device(&mut self) -> Result<HSYNTHETICPOINTERDEVICE, String> {
        if let Some(device) = self.pen {
            return Ok(device);
        }
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT) }
            .map_err(|error| format!("pen_unavailable: {error}"))?;
        self.pen = Some(device);
        Ok(device)
    }

    fn pen_event(&mut self, flags: POINTER_FLAGS, x: i32, y: i32, pen: Pen) -> Result<(), String> {
        let (sx, sy) = self.screen(x, y)?;
        let device = self.pen_device()?;
        let info = POINTER_TYPE_INFO {
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
                    penFlags: (if pen.barrel { PEN_FLAG_BARREL } else { 0 })
                        | (if pen.eraser { PEN_FLAG_ERASER } else { 0 }),
                    penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                    pressure: pen.pressure,
                    rotation: 0,
                    tiltX: pen.tilt_x,
                    tiltY: pen.tilt_y,
                },
            },
        };
        unsafe { InjectSyntheticPointerInput(device, &[info]) }
            .map_err(|error| format!("input_failed: pen injection: {error}"))
    }
}

impl Backend for Native {
    fn check_target(&mut self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("cancelled".into());
        }
        crate::human_input::check()?;
        if crate::win32::window_owner_pid(self.hwnd) != Some(self.pid)
            || !unsafe { IsWindowVisible(HWND(self.hwnd as *mut _)) }.as_bool()
            || crate::input::window_is_iconic(self.hwnd)
        {
            return Err("target_lost: exact window is gone, hidden or minimized".into());
        }
        let actual = unsafe { GetForegroundWindow() }.0 as usize as u64;
        if !crate::win32::foreground_matches_target_or_owned_window(self.foreground, actual) {
            return Err("foreground_lost: stopped before another timeline event".into());
        }
        Ok(())
    }

    fn wait_until(&mut self, at_ms: u64) -> Result<(), String> {
        let deadline = Duration::from_millis(at_ms);
        while self.started.elapsed() < deadline {
            self.check_target()?;
            sleep((deadline - self.started.elapsed().min(deadline)).min(Duration::from_millis(5)));
        }
        Ok(())
    }

    fn send(&mut self, event: &Event) -> Result<(), String> {
        match event {
            Event::KeyDown(key) | Event::KeyUp(key) => {
                let vk = crate::input::keyboard::key_name_to_vk(key)
                    .map_err(|error| format!("invalid_timeline_key: {error}"))?;
                self.insert(crate::input::keyboard::key_input(
                    vk,
                    matches!(event, Event::KeyUp(_)),
                ))
            }
            Event::ButtonDown(button) | Event::ButtonUp(button) => {
                let (flags, data) = button_flags(*button, matches!(event, Event::ButtonDown(_)));
                self.insert(mouse(flags, 0, 0, data))
            }
            Event::Move { x, y } => self.move_absolute(*x, *y),
            Event::MoveRelative { dx, dy } => self.insert(mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE,
                *dx,
                *dy,
                0,
            )),
            Event::Wheel { dx, dy } => {
                if *dy != 0 {
                    // Positive detents scroll down, as in scroll_at; Windows counts wheel-up as positive.
                    self.insert(mouse(MOUSEEVENTF_WHEEL, 0, 0, (-dy * 120) as u32))?;
                }
                if *dx != 0 {
                    self.insert(mouse(MOUSEEVENTF_HWHEEL, 0, 0, (dx * 120) as u32))?;
                }
                Ok(())
            }
            Event::PenDown { x, y, pen } => {
                // Bring the pen into range above the point before contact so apps see a normal stroke start.
                self.pen_event(POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE, *x, *y, *pen)?;
                self.pen_event(
                    POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
                    *x,
                    *y,
                    *pen,
                )
            }
            Event::PenMove { x, y, pen } => self.pen_event(
                POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
                *x,
                *y,
                *pen,
            ),
            Event::PenUp { x, y, pen } => {
                self.pen_event(POINTER_FLAG_UP | POINTER_FLAG_INRANGE, *x, *y, *pen)?;
                self.pen_event(POINTER_FLAG_UPDATE, *x, *y, *pen)
            }
        }
    }

    fn release(
        &mut self,
        keys: &[String],
        buttons: &[Button],
        pen: Option<(i32, i32, Pen)>,
    ) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Some((x, y, p)) = pen {
            if let Err(error) = self
                .pen_event(POINTER_FLAG_UP | POINTER_FLAG_INRANGE, x, y, p)
                .and_then(|()| self.pen_event(POINTER_FLAG_UPDATE, x, y, p))
            {
                errors.push(error);
            }
        }
        for button in buttons.iter().rev() {
            let (flags, data) = button_flags(*button, false);
            if let Err(error) = self.insert(mouse(flags, 0, 0, data)) {
                errors.push(error);
            }
        }
        for key in keys.iter().rev() {
            match crate::input::keyboard::key_name_to_vk(key) {
                Ok(vk) => {
                    if let Err(error) = self.insert(crate::input::keyboard::key_input(vk, true)) {
                        errors.push(error);
                    }
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("release_failed: {}", errors.join("; ")))
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        if let Some(device) = self.pen.take() {
            unsafe { DestroySyntheticPointerDevice(device) };
        }
    }
}

/// Bring the exact window forward, then run the whole program. Everything still held is released at the end.
pub(crate) fn send_timeline(
    hwnd: u64,
    pid: u32,
    program: &[Timed],
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
) -> Result<Report, String> {
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
    for vk in [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2] {
        if unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16 & 0x8000 != 0 {
            return Err("input_busy: a mouse button is already held".into());
        }
    }
    let mut native = Native {
        hwnd,
        pid,
        cancelled,
        map,
        started: Instant::now(),
        pen: None,
        foreground: crate::win32::capture_foreground_target(hwnd)
            .ok_or("target_lost: exact window disappeared before input")?,
    };
    for timed in program {
        match &timed.event {
            Event::Move { x, y }
            | Event::PenDown { x, y, .. }
            | Event::PenMove { x, y, .. }
            | Event::PenUp { x, y, .. } => {
                native.screen(*x, *y)?;
            }
            Event::KeyDown(key) => {
                crate::input::keyboard::key_name_to_vk(key)
                    .map_err(|error| format!("invalid_timeline_key: {error}"))?;
            }
            _ => {}
        }
    }
    if !unsafe { crate::input::force_foreground_assisted(HWND(hwnd as *mut _)) }.0 {
        return Err("foreground_unavailable: exact window did not become foreground".into());
    }
    native.started = Instant::now();
    let mut report = timeline::execute(program, &mut native);
    report.elapsed_ms = native.started.elapsed().as_millis() as u64;
    Ok(report)
}
