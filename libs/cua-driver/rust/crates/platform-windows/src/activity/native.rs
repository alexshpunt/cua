//! Protected HUD windows are created/destroyed on one thread; decode runs separately.

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
// This lock protects HWND lifetime, not input or pixel preparation. Invalidation
// revokes a surface's global alpha; destruction stays on the owner thread.
static HUDS: OnceLock<Mutex<HashMap<String, isize>>> = OnceLock::new();
fn huds() -> &'static Mutex<HashMap<String, isize>> {
    HUDS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub(super) fn hide(key: &str) -> Result<(), &'static str> {
    let Some(registry) = HUDS.get() else {
        return Ok(());
    };
    let registry = registry.lock().map_err(|_| "visual_state_unavailable")?;
    let Some(&raw) = registry.get(key) else {
        return Ok(());
    };
    unsafe {
        // A revoked surface is never uploaded again. Windows deliberately rejects
        // later UpdateLayeredWindow calls after this global-alpha operation.
        SetLayeredWindowAttributes(
            HWND(raw as *mut _),
            windows::Win32::Foundation::COLORREF(0),
            0,
            LWA_ALPHA,
        )
        .map_err(|_| "native_hud_clear_failed")?;
        DwmFlush().map_err(|_| "native_hud_clear_failed")?;
    }
    Ok(())
}
struct Active {
    key: String,
    hwnd: HWND,
    surface: WinSurface,
    work: Work,
    preview: Option<tiny_skia::Pixmap>,
    cued: bool,
}
impl Drop for Active {
    fn drop(&mut self) {
        // Recovery keeps cleanup possible if another cleanup operation panicked.
        let mut registry = huds()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry.get(&self.key) == Some(&(self.hwnd.0 as isize)) {
            registry.remove(&self.key);
        }
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
                || crate::uia::windows_enum::window_is_cloaked(hwnd)
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
struct Prepared {
    key: String,
    work: Work,
    preview: Option<tiny_skia::Pixmap>,
}
fn prepare(key: String, mut work: Work, settings: VisualSettings) -> Option<Prepared> {
    if !work.ready(Instant::now())
        || with_current(&key, &work, |_| ()).is_none()
        || !source_valid(work.event.source.as_ref()?)
    {
        return None;
    }
    let bounds = work.event.source.as_ref()?.bounds();
    if u64::from(bounds.width) * u64::from(bounds.height) > MAX_SURFACE_PIXELS {
        return None;
    }
    if !work.ready(Instant::now()) {
        return None;
    }
    let preview = if settings.previews() {
        work.event.image.as_ref().and_then(decode_preview)
    } else {
        None
    };
    work.event.image = None;
    if !work.ready(Instant::now()) || with_current(&key, &work, |_| ()).is_none() {
        return None;
    }
    Some(Prepared { key, work, preview })
}
fn create(class: &[u16], prepared: Prepared) -> Option<Active> {
    let Prepared { key, work, preview } = prepared;
    if !work.ready(Instant::now()) {
        return None;
    }
    let source = work.event.source.as_ref()?;
    if !source_valid(source) {
        return None;
    }
    let b = source.bounds();
    if u64::from(b.width) * u64::from(b.height) > MAX_SURFACE_PIXELS {
        return None;
    }
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
        huds()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), hwnd.0 as isize);
        Some(Active {
            key,
            hwnd,
            surface,
            work,
            preview,
            cued: false,
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
        let command = if matches!(action, CursorAction::Scroll | CursorAction::Drag) {
            cursor_overlay::track_pointer_command(f64::from(p.x), f64::from(p.y))
        } else {
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
            }
        };
        crate::overlay::send_command(key.to_owned(), command);
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
    // At most one bounded image decode is in flight. It must never monopolize
    // the window-owner loop that expires and destroys already-visible surfaces.
    let mut preparing: Option<std::thread::JoinHandle<Option<Prepared>>> = None;
    loop {
        let interval = active
            .keys()
            .map(|key| settings(key).frame_ms())
            .min()
            .unwrap_or(if preparing.is_some() { 16 } else { 250 });
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
        active.retain(|key, item| with_current(key, &item.work, |_| ()).is_some());
        if preparing.as_ref().is_some_and(|job| job.is_finished()) {
            if let Some(Ok(Some(prepared))) = preparing.take().map(|job| job.join()) {
                let key = prepared.key.clone();
                if prepared.work.ready(Instant::now())
                    && with_current(&key, &prepared.work, |_| ()).is_some()
                {
                    active.remove(&key);
                    if active.len() >= MAX_PENDING_SOURCES {
                        if let Some(oldest) = active
                            .iter()
                            .min_by_key(|(_, v)| v.work.admitted)
                            .map(|(k, _)| k.clone())
                        {
                            active.remove(&oldest);
                        }
                    }
                    if let Some(item) = create(&class, prepared) {
                        active.insert(key, item);
                    }
                }
            }
        }
        if preparing.is_none() {
            let next = {
                let Ok(mut state) = state().lock() else {
                    return;
                };
                let now = Instant::now();
                for session in state.sessions.values_mut() {
                    if session
                        .pending
                        .as_ref()
                        .is_some_and(|work| !work.ready(now) || !session.current(work, now))
                    {
                        session.pending = None;
                    }
                }
                let key = state
                    .sessions
                    .iter()
                    .filter_map(|(key, session)| {
                        session
                            .pending
                            .as_ref()
                            .map(|work| (key.clone(), work.admitted))
                    })
                    .max_by_key(|(_, admitted)| *admitted)
                    .map(|(key, _)| key);
                key.and_then(|key| {
                    let session = state.sessions.get_mut(&key)?;
                    Some((key, session.pending.take()?, session.settings.clone()))
                })
            };
            if let Some((key, work, settings)) = next {
                preparing = match std::thread::Builder::new()
                    .name("cua-preview-prepare".into())
                    .spawn(move || prepare(key, work, settings))
                {
                    Ok(job) => Some(job),
                    Err(error) => {
                        tracing::warn!(%error, "Native preview preparation was unavailable");
                        None
                    }
                };
            }
        }
        active.retain(|key, item| {
            let Active {
                hwnd,
                surface,
                work,
                preview,
                cued,
                ..
            } = item;
            // This barrier spans the final check, pixels and cursor cues. Invalidation
            // waits for this commit, then clears any effects it enqueued.
            with_current(key, work, |settings| {
                let Some(source) = work.event.source.as_ref() else {
                    return false;
                };
                if !source_valid(source) {
                    return false;
                }
                if !settings.previews() {
                    *preview = None;
                }
                surface.pixmap.fill(tiny_skia::Color::TRANSPARENT);
                paint_activity(
                    &mut surface.pixmap,
                    &work.event,
                    preview.as_ref(),
                    &settings,
                    work.admitted.elapsed().as_millis() as u64,
                    cursor_overlay::session_fill_rgba(key),
                );
                let now = Instant::now();
                if (!*cued && !work.ready(now))
                    || now.saturating_duration_since(work.admitted).as_millis()
                        >= u128::from(TTL_MS)
                {
                    return false;
                }
                unsafe {
                    if !surface.present_hud(*hwnd) {
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
                    // Upload/swizzle may be expensive. Revalidate immediately
                    // before showing the hidden surface or claiming a cursor cue.
                    if !source_valid(source) {
                        return false;
                    }
                    let now = Instant::now();
                    if (!*cued && !work.ready(now))
                        || now.saturating_duration_since(work.admitted).as_millis()
                            >= u128::from(TTL_MS)
                    {
                        return false;
                    }
                    WinZOrderEnforcer {
                        hwnd_isize: hwnd.0 as isize,
                    }
                    .reassert(target);
                    if GetForegroundWindow() != previous {
                        return false;
                    }
                }
                let now = Instant::now();
                if (!*cued && !work.ready(now))
                    || now.saturating_duration_since(work.admitted).as_millis()
                        >= u128::from(TTL_MS)
                {
                    return false;
                }
                if !*cued {
                    cue(key, &work.event);
                    *cued = true;
                }
                true
            })
            .unwrap_or(false)
        });
    }
}
