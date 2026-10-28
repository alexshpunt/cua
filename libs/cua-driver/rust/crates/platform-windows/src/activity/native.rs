//! Protected native HUD surfaces. Each is owned and destroyed on this one worker thread.

use super::*;
use crate::overlay::{WinSurface, WinZOrderEnforcer};
use cursor_overlay::activity::{
    decode_preview, identity_property, paint_activity, ActivityRect, ActivitySource, TTL_MS,
};
use cursor_overlay::ZOrderEnforcer;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmFlush, DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const MAX_SURFACE_PIXELS: u64 = 8 * 1024 * 1024;
struct Active {
    hwnd: HWND,
    surface: WinSurface,
    event: ActivityEvent,
    preview: Option<tiny_skia::Pixmap>,
    born: Instant,
}
impl Drop for Active {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn native_rect(rect: RECT) -> Option<ActivityRect> {
    let w = rect.right.checked_sub(rect.left)?;
    let h = rect.bottom.checked_sub(rect.top)?;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(ActivityRect {
        x: rect.left,
        y: rect.top,
        width: w as u32,
        height: h as u32,
    })
}

/// Recheck identity and exact geometry each rendered frame. A moved/resized source expires its event.
fn source_valid(source: &ActivitySource) -> bool {
    match source {
        ActivitySource::Window {
            window_id,
            pid,
            identity,
            bounds,
        } => unsafe {
            let hwnd = HWND(*window_id as *mut _);
            if !IsWindow(hwnd).as_bool()
                || !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
            {
                return false;
            }
            let mut current_pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut current_pid));
            let Some((property, expected)) = identity_property(identity, *pid) else {
                return false;
            };
            let property: Vec<u16> = property.encode_utf16().chain(Some(0)).collect();
            if current_pid != *pid
                || GetPropW(hwnd, PCWSTR(property.as_ptr())).0 as usize != expected
            {
                return false;
            }
            let mut rect = RECT::default();
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut _ as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
            .is_err()
            {
                return false;
            }
            native_rect(rect) == Some(*bounds)
        },
        ActivitySource::Display { display, bounds } => {
            crate::display::enumerate().ok().is_some_and(|topology| {
                topology.displays.iter().any(|d| {
                    (d.display_id == *display || (display == "primary" && d.primary))
                        && d.bounds.x == bounds.x
                        && d.bounds.y == bounds.y
                        && d.bounds.width == bounds.width
                        && d.bounds.height == bounds.height
                })
            })
        }
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
fn create(class: &[u16], event: ActivityEvent, settings: &VisualSettings) -> Option<Active> {
    let source = event.source.as_ref()?;
    if !source_valid(source) {
        return None;
    }
    let b = source.bounds();
    if u64::from(b.width) * u64::from(b.height) > MAX_SURFACE_PIXELS {
        return None;
    }
    // Sensitive image decode occurs only after trusted source validation and never on the tool thread.
    let preview = if settings.previews() {
        event.image.as_ref().and_then(decode_preview)
    } else {
        None
    };
    let mut event = event;
    event.image = None;
    unsafe {
        let instance = GetModuleHandleW(PCWSTR::null()).ok()?;
        let hwnd = CreateWindowExW(
            WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_POPUP,
            b.x,
            b.y,
            b.width as i32,
            b.height as i32,
            None,
            None,
            instance,
            None,
        )
        .ok()?;
        // Never show even one unprotected frame. The cursor uses a different existing window,
        // so its recorder visibility policy remains unchanged.
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        for _ in 0..2 {
            if DwmFlush().is_err() {
                let _ = DestroyWindow(hwnd);
                return None;
            }
        }
        let surface = match WinSurface::create(b.x, b.y, b.width as i32, b.height as i32) {
            Some(s) => s,
            None => {
                let _ = DestroyWindow(hwnd);
                return None;
            }
        };
        Some(Active {
            hwnd,
            surface,
            event,
            preview,
            born: Instant::now(),
        })
    }
}

fn cue(key: &str, event: &ActivityEvent) {
    if event.kind != ActivityKind::Action || event.outcome.as_deref() == Some("refused") {
        return;
    }
    let action = match event.action.as_deref() {
        Some("type_text" | "type" | "fill") => CursorAction::Text,
        Some("press_key" | "hotkey" | "keypress") => CursorAction::Key,
        Some("scroll" | "scroll_at") => CursorAction::Scroll,
        Some("drag" | "input_timeline") => CursorAction::Drag,
        Some("hover" | "move_pointer") => CursorAction::Navigate,
        _ => CursorAction::Click,
    };
    crate::overlay::send_command(
        key.to_owned(),
        OverlayCommand::BeginAction {
            action,
            delivery: None,
            target: None,
        },
    );
    if let Some(p) = event.point {
        crate::overlay::send_command(
            key.to_owned(),
            OverlayCommand::MoveTo {
                x: f64::from(p.x),
                y: f64::from(p.y),
                end_heading_radians: std::f64::consts::FRAC_PI_4,
                target: event.region.map(|r| {
                    [
                        f64::from(r.x),
                        f64::from(r.y),
                        f64::from(r.width),
                        f64::from(r.height),
                    ]
                }),
            },
        );
        if action == CursorAction::Click {
            crate::overlay::send_command(
                key.to_owned(),
                OverlayCommand::ClickPulse {
                    x: f64::from(p.x),
                    y: f64::from(p.y),
                },
            );
        }
    }
    crate::overlay::send_command(key.to_owned(), OverlayCommand::EndAction(action));
}

pub(super) fn run(rx: Receiver<()>) {
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // Thread-only PMv2 coordinates. This worker exits without touching process/machine DPI settings.
    if unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .0
        .is_null()
    {
        return;
    }
    let class: Vec<u16> = "Cua.ProtectedActivityHud\0".encode_utf16().collect();
    unsafe {
        let Ok(instance) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&window_class);
    }
    let mut active: HashMap<String, Active> = HashMap::new();
    loop {
        let interval = active
            .keys()
            .map(|key| settings(key).frame_ms())
            .min()
            .unwrap_or(250);
        match rx.recv_timeout(Duration::from_millis(interval)) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            _ => {}
        }
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let pending = {
            let Ok(mut state) = state().lock() else {
                return;
            };
            active.retain(|key, item| {
                state.sessions.get(key).is_some_and(|s| {
                    s.generation == item.event.generation
                        && s.settings.enabled()
                        && (s.settings.observations || item.event.kind == ActivityKind::Action)
                }) && item.born.elapsed().as_millis() < u128::from(TTL_MS)
            });
            state
                .sessions
                .iter_mut()
                .filter_map(|(key, s)| {
                    s.pending
                        .take()
                        .map(|event| (key.clone(), event, s.settings.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (key, event, settings) in pending {
            // Replace only this session's previous image. A failed/stale replacement must not retain it.
            active.remove(&key);
            if !settings.enabled() {
                continue;
            }
            if active.len() >= MAX_PENDING_SOURCES {
                if let Some(oldest) = active
                    .iter()
                    .min_by_key(|(_, v)| v.born)
                    .map(|(k, _)| k.clone())
                {
                    active.remove(&oldest);
                }
            }
            if let Some(item) = create(&class, event, &settings) {
                let current = state().lock().ok().is_some_and(|s| {
                    s.sessions.get(&key).is_some_and(|s| {
                        s.generation == item.event.generation && s.settings.enabled()
                    })
                });
                if current {
                    cue(&key, &item.event);
                    active.insert(key, item);
                }
            }
        }
        active.retain(|key, item| {
            let Some(source) = item.event.source.as_ref() else {
                return false;
            };
            if !source_valid(source) {
                return false;
            }
            let settings = settings(key);
            if !settings.enabled()
                || (!settings.observations && item.event.kind != ActivityKind::Action)
            {
                return false;
            }
            if item.event.kind == ActivityKind::Action {
                let allowed = match item.event.action.as_deref() {
                    Some("type_text" | "type" | "fill" | "press_key" | "hotkey" | "keypress") => {
                        settings.keyboard
                    }
                    Some("scroll" | "scroll_at" | "drag" | "input_timeline") => {
                        settings.scroll_drag
                    }
                    _ => settings.pointer,
                };
                if !allowed {
                    return false;
                }
            }
            if !settings.previews() {
                item.preview = None;
            }
            item.surface.pixmap.fill(tiny_skia::Color::TRANSPARENT);
            paint_activity(
                &mut item.surface.pixmap,
                &item.event,
                item.preview.as_ref(),
                &settings,
                item.born.elapsed().as_millis() as u64,
                cursor_overlay::session_fill_rgba(key),
            );
            unsafe {
                if !item.surface.present_hud(item.hwnd) {
                    return false;
                }
                let previous = GetForegroundWindow();
                let target = match source {
                    ActivitySource::Window { window_id, .. } => {
                        let root = GetAncestor(HWND(*window_id as *mut _), GA_ROOT);
                        Some(if root.0.is_null() {
                            *window_id
                        } else {
                            root.0 as u64
                        })
                    }
                    _ => None,
                };
                // Reuse the cursor's nonactivating z-order policy, including
                // topmost demotion and stable self-sibling handling.
                WinZOrderEnforcer {
                    hwnd_isize: item.hwnd.0 as isize,
                }
                .reassert(target);
                if GetForegroundWindow() != previous {
                    return false;
                }
            }
            true
        });
    }
}
