//! Exact-window native mouse motion, followed by bounded dwell or checked wheel axes.
//! Unlike click/drag helpers, this operation leaves focus and pointer in place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use cua_driver_core::pointer::{self, Backend, Failure, Report};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU,
    VK_RBUTTON, VK_RWIN, VK_SHIFT, VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    IsWindowVisible, WindowFromPoint, GA_ROOT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

type MapPoint = fn(u64, i32, i32) -> (i32, i32);

fn failure(code: &'static str, detail: impl Into<String>) -> Failure {
    Failure {
        code,
        detail: detail.into(),
    }
}

fn cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

fn foreground_window() -> u64 {
    unsafe { GetForegroundWindow() }.0 as usize as u64
}

/// Geometry frozen before capture/token admission, never renewed after it.
pub(crate) struct PointerGeometry {
    bounds: RECT,
    origin: (i32, i32),
}

impl PointerGeometry {
    pub(crate) fn read(hwnd: u64, map: MapPoint) -> Result<Self, Failure> {
        let mut bounds = RECT::default();
        unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut bounds) }
            .map_err(|error| failure("geometry_unavailable", error.to_string()))?;
        if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
            return Err(failure("geometry_unavailable", "Window bounds are empty"));
        }
        Ok(Self {
            bounds,
            origin: map(hwnd, 0, 0),
        })
    }

    pub(crate) fn local_point(&self, screen: (i32, i32)) -> Result<(i32, i32), Failure> {
        Ok((
            screen.0.checked_sub(self.origin.0).ok_or_else(|| {
                failure(
                    "element_geometry_unavailable",
                    "Element X coordinate overflow",
                )
            })?,
            screen.1.checked_sub(self.origin.1).ok_or_else(|| {
                failure(
                    "element_geometry_unavailable",
                    "Element Y coordinate overflow",
                )
            })?,
        ))
    }
}
/// Native delivery prefix plus independently measured pointer/focus effects.
pub(crate) struct Outcome<R = Report> {
    pub report: R,
    pub activation_attempted: bool,
    pub foreground_before: u64,
    pub foreground_after: u64,
    pub pointer_before: Option<(i32, i32)>,
    pub pointer_after: Option<(i32, i32)>,
    pub pointer_target: Option<(i32, i32)>,
}

struct NativePointer {
    hwnd: u64,
    pid: u32,
    local_point: (i32, i32),
    map: MapPoint,
    origin: (i32, i32),
    bounds: RECT,
    cancelled: Arc<AtomicBool>,
    started: Option<Instant>,
    delivered_point: Option<(i32, i32)>,
    arrival_pending: bool,
    block_modifiers: bool,
}

impl NativePointer {
    fn owns_point(&self, point: (i32, i32)) -> bool {
        if point.0 < self.bounds.left
            || point.0 >= self.bounds.right
            || point.1 < self.bounds.top
            || point.1 >= self.bounds.bottom
            || !crate::input::point_on_virtual_desktop(point.0, point.1)
        {
            return false;
        }
        let hit = unsafe {
            WindowFromPoint(POINT {
                x: point.0,
                y: point.1,
            })
        };
        !hit.0.is_null() && unsafe { GetAncestor(hit, GA_ROOT) } == HWND(self.hwnd as *mut _)
    }

    fn confirm_arrival(&mut self) -> Result<(), Failure> {
        let deadline = Instant::now() + Duration::from_millis(50);
        loop {
            self.check_identity()?;
            if foreground_window() != self.hwnd {
                return Err(failure(
                    "foreground_lost",
                    "Foreground changed while confirming pointer arrival",
                ));
            }
            let target = self.screen_point()?;
            if let Some(actual) = cursor_position() {
                // The shared virtual-desktop mapping recovers within one pixel.
                // Accept quantization only after checking the actual destination's owner.
                if (i64::from(actual.0) - i64::from(target.0)).abs() <= 1
                    && (i64::from(actual.1) - i64::from(target.1)).abs() <= 1
                {
                    if !self.owns_point(actual) {
                        return Err(failure(
                            "pointer_arrival_unproven",
                            "The actual pointer destination is not owned by the exact window",
                        ));
                    }
                    self.delivered_point = Some(actual);
                    self.started = Some(Instant::now());
                    self.arrival_pending = false;
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(failure(
                    "pointer_arrival_unproven",
                    "Pointer arrival was not confirmed within 50 ms; movement was not replayed",
                ));
            }
            sleep(Duration::from_millis(2));
        }
    }
    fn screen_point(&self) -> Result<(i32, i32), Failure> {
        if (self.map)(self.hwnd, 0, 0) != self.origin {
            return Err(failure(
                "window_geometry_changed",
                "Capture origin changed during pointer delivery",
            ));
        }
        let origin = self.origin;
        let x = origin
            .0
            .checked_add(self.local_point.0)
            .ok_or_else(|| failure("pointer_outside_window", "X coordinate overflow"))?;
        let y = origin
            .1
            .checked_add(self.local_point.1)
            .ok_or_else(|| failure("pointer_outside_window", "Y coordinate overflow"))?;
        if x < self.bounds.left
            || x >= self.bounds.right
            || y < self.bounds.top
            || y >= self.bounds.bottom
            || !crate::input::point_on_virtual_desktop(x, y)
        {
            return Err(failure(
                "pointer_outside_window",
                "Point is outside the exact window or display",
            ));
        }
        Ok((x, y))
    }

    fn check_identity(&self) -> Result<(), Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(failure("cancelled", "Pointer operation was cancelled"));
        }
        let hwnd = HWND(self.hwnd as *mut _);
        if crate::win32::window_owner_pid(self.hwnd) != Some(self.pid)
            || !unsafe { IsWindowVisible(hwnd) }.as_bool()
            || crate::input::window_is_iconic(self.hwnd)
            || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
        {
            return Err(failure(
                "target_lost",
                "Exact top-level window is gone, hidden or minimized",
            ));
        }
        let mut bounds = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut bounds) }
            .map_err(|error| failure("geometry_unavailable", error.to_string()))?;
        if bounds != self.bounds {
            return Err(failure(
                "window_geometry_changed",
                "Window geometry changed during pointer delivery",
            ));
        }
        if self.block_modifiers
            && [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                .iter()
                .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000 != 0)
        {
            return Err(failure(
                "input_busy",
                "A modifier is held; wheel input could become zoom or another gesture",
            ));
        }
        if [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]
            .iter()
            .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000 != 0)
        {
            return Err(failure(
                "input_busy",
                "A mouse button is held; movement could become a drag",
            ));
        }
        Ok(())
    }
}

impl Backend for NativePointer {
    fn check_target(&mut self) -> Result<(), Failure> {
        crate::human_input::check().map_err(|detail| failure("human_takeover", detail))?;
        self.check_identity()?;
        if foreground_window() != self.hwnd {
            return Err(failure(
                "foreground_lost",
                "Exact target no longer owns foreground",
            ));
        }
        if !self.owns_point(self.screen_point()?) {
            return Err(failure(
                "pointer_target_occluded",
                "The point is covered by another window",
            ));
        }
        if self.arrival_pending {
            self.confirm_arrival()?;
        }
        if self
            .delivered_point
            .is_some_and(|point| !self.owns_point(point))
        {
            return Err(failure(
                "pointer_target_occluded",
                "Confirmed pointer destination is now covered by another window",
            ));
        }
        if self.delivered_point.is_some() && cursor_position() != self.delivered_point {
            return Err(failure(
                "pointer_moved",
                "The OS pointer moved during dwell; no input was replayed",
            ));
        }
        Ok(())
    }

    fn move_once(&mut self) -> Result<(), Failure> {
        // Check immediately before insertion, not only before the worker starts.
        self.check_target()?;
        let (x, y) = self.screen_point()?;
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
        self.check_target()?;
        if self.cancelled.load(Ordering::Acquire) {
            return Err(failure("cancelled", "Cancelled before movement insertion"));
        }
        self.started = None;
        let inserted = unsafe {
            SendInput(
                &[INPUT {
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
                }],
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if inserted != 1 {
            return Err(failure(
                "pointer_input_failed",
                "SendInput inserted 0/1 movement events",
            ));
        }
        self.arrival_pending = true;
        Ok(())
    }

    fn wait_until(&mut self, elapsed_ms: u64) -> Result<(), Failure> {
        let started = self.started.ok_or_else(|| {
            failure(
                "pointer_arrival_unproven",
                "Dwell requires confirmed native arrival",
            )
        })?;
        let deadline = Duration::from_millis(elapsed_ms);
        while started.elapsed() < deadline {
            self.check_target()?;
            sleep((deadline - started.elapsed().min(deadline)).min(Duration::from_millis(10)));
        }
        Ok(())
    }

    fn elapsed_ms(&self) -> u64 {
        self.started
            .map(|started| started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }
}

impl cua_driver_core::scroll::Backend for NativePointer {
    fn check_target(&mut self) -> Result<(), Failure> {
        Backend::check_target(self)
    }

    fn move_once(&mut self) -> Result<(), Failure> {
        Backend::move_once(self)
    }

    fn scroll_once(
        &mut self,
        axis: cua_driver_contract::ScrollAxis,
        detents: i32,
    ) -> Result<(), Failure> {
        Backend::check_target(self)?;
        let (flag, signed_delta) = match axis {
            cua_driver_contract::ScrollAxis::X => (MOUSEEVENTF_HWHEEL, detents * 120),
            cua_driver_contract::ScrollAxis::Y => (MOUSEEVENTF_WHEEL, -detents * 120),
        };
        // Insertion is deliberately separate per axis so cancellation retains a precise prefix.
        // Native ownership checking and SendInput are not atomic against external desktop changes.
        if self.cancelled.load(Ordering::Acquire) {
            return Err(failure("cancelled", "Cancelled before wheel insertion"));
        }
        let inserted = unsafe {
            SendInput(
                &[INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            mouseData: signed_delta as u32,
                            dwFlags: flag,
                            ..Default::default()
                        },
                    },
                }],
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if inserted != 1 {
            return Err(failure(
                "scroll_input_failed",
                "SendInput inserted 0/1 wheel events",
            ));
        }
        Ok(())
    }
}

fn native_pointer(
    hwnd: u64,
    pid: u32,
    local_point: (i32, i32),
    geometry: PointerGeometry,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
    block_modifiers: bool,
) -> NativePointer {
    NativePointer {
        hwnd,
        pid,
        local_point,
        origin: geometry.origin,
        map,
        bounds: geometry.bounds,
        cancelled,
        started: None,
        delivered_point: None,
        arrival_pending: false,
        block_modifiers,
    }
}

fn run_operation<R>(
    mut native: NativePointer,
    execute: impl FnOnce(&mut NativePointer) -> R,
    refused: impl FnOnce(Failure) -> R,
) -> Outcome<R> {
    let foreground_before = foreground_window();
    let pointer_before = cursor_position();
    let mut activation_attempted = false;
    let mut pointer_target = None;
    let result = (|| {
        native.check_identity()?;
        pointer_target = Some(native.screen_point()?);
        if let Some(error) = crate::input::post_message_blocked_by_uipi(native.hwnd) {
            return Err(failure("input_integrity_denied", error));
        }
        native.check_identity()?;
        native.screen_point()?;
        if native.cancelled.load(Ordering::Acquire) {
            return Err(failure(
                "cancelled",
                "Cancelled before foreground activation",
            ));
        }
        if foreground_before != native.hwnd {
            activation_attempted = true;
            // The assisted helper injects VK_NONAME. These actions never inject keys.
            if !unsafe { crate::input::force_foreground_attached(HWND(native.hwnd as *mut _)) } {
                return Err(failure(
                    "foreground_unavailable",
                    "Exact target did not become foreground",
                ));
            }
        }
        let report = execute(&mut native);
        if let Some(confirmed) = native.delivered_point {
            pointer_target = Some(confirmed);
        }
        Ok(report)
    })();
    Outcome {
        report: result.unwrap_or_else(refused),
        activation_attempted,
        foreground_before,
        foreground_after: foreground_window(),
        pointer_before,
        pointer_after: cursor_position(),
        pointer_target,
    }
}

/// Use one native admission. Never restore focus/pointer or synthesize keys/buttons.
pub(crate) fn send_motion(
    hwnd: u64,
    pid: u32,
    local_point: (i32, i32),
    dwell_ms: u64,
    geometry: PointerGeometry,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
) -> Outcome {
    run_operation(
        native_pointer(hwnd, pid, local_point, geometry, cancelled, map, false),
        |native| pointer::execute(dwell_ms, native),
        |error| Report {
            attempted: false,
            delivered: false,
            dwell_elapsed_ms: 0,
            failure: Some(error),
        },
    )
}

/// Move once to an admitted point, then insert bounded wheel deltas with no restoration.
pub(crate) fn send_scroll(
    hwnd: u64,
    pid: u32,
    local_point: (i32, i32),
    deltas: (i32, i32),
    geometry: PointerGeometry,
    cancelled: Arc<AtomicBool>,
    map: MapPoint,
) -> Outcome<cua_driver_core::scroll::Report> {
    run_operation(
        native_pointer(hwnd, pid, local_point, geometry, cancelled, map, true),
        |native| cua_driver_core::scroll::execute(deltas.0, deltas.1, native),
        |error| cua_driver_core::scroll::Report {
            pointer_attempted: false,
            pointer_delivered: false,
            attempted_axes: Vec::new(),
            delivered_axes: Vec::new(),
            failure: Some(error),
        },
    )
}
