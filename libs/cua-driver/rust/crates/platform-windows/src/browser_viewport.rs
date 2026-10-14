//! Read-only native Chromium viewport measurements. No chrome-inset guesses.
use cua_driver_core::browser::{
    dom_geometry::NativeBrowserViewport,
    refusal::{BrowserRefusal, BrowserRefusalCode},
    types::Rect,
};
use windows::Win32::{
    Foundation::{BOOL, FALSE, HWND, LPARAM, POINT, RECT, TRUE},
    Graphics::Gdi::ClientToScreen,
    UI::{
        HiDpi::{AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
        WindowsAndMessaging::{EnumChildWindows, GetAncestor, GetClassNameW, GetClientRect,
            GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, GA_ROOT},
    },
};

fn unavailable(reason: &str) -> BrowserRefusal {
    BrowserRefusal::new(BrowserRefusalCode::BrowserRouteUnavailable, reason)
}

struct Scan {
    root: HWND,
    visited: usize,
    exhausted: bool,
    candidates: Vec<(HWND, Rect)>,
}

unsafe fn client_bounds(hwnd: HWND) -> Option<Rect> {
    let mut bounds = RECT::default();
    GetClientRect(hwnd, &mut bounds).ok()?;
    if bounds.right <= bounds.left || bounds.bottom <= bounds.top { return None; }
    let mut origin = POINT { x: bounds.left, y: bounds.top };
    if !ClientToScreen(hwnd, &mut origin).as_bool() { return None; }
    Some(Rect { x: origin.x as f64, y: origin.y as f64,
        width: (bounds.right - bounds.left) as f64,
        height: (bounds.bottom - bounds.top) as f64 })
}

unsafe extern "system" fn child(hwnd: HWND, param: LPARAM) -> BOOL {
    let scan = &mut *(param.0 as *mut Scan);
    scan.visited += 1;
    if scan.visited > 1024 { scan.exhausted = true; return FALSE; }
    if GetAncestor(hwnd, GA_ROOT) != scan.root || !IsWindowVisible(hwnd).as_bool() { return TRUE; }
    let mut class = [0u16; 128];
    let length = GetClassNameW(hwnd, &mut class);
    if length > 0 && String::from_utf16_lossy(&class[..length as usize]) == "Chrome_RenderWidgetHostHWND" {
        if let Some(bounds) = client_bounds(hwnd) { scan.candidates.push((hwnd, bounds)); }
    }
    TRUE
}

fn owned(hwnd: HWND, pid: u32) -> bool {
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)); }
    owner == pid && owner != 0
}

/// Return the sole visible render widget beneath the exact browser window.
/// A selected-tab proof belongs to core; multiple widgets, changed geometry,
/// minimized windows and non-physical thread coordinates refuse explicitly.
pub(crate) fn measure(pid: i64, window: u64) -> Result<NativeBrowserViewport, BrowserRefusal> {
    let pid = u32::try_from(pid).map_err(|_| unavailable("invalid browser owner"))?;
    let root = HWND(window as *mut _);
    unsafe {
        if !AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool() {
            return Err(unavailable("native viewport needs physical per-monitor-v2 coordinates"));
        }
        if !owned(root, pid) || IsIconic(root).as_bool() || !IsWindowVisible(root).as_bool() {
            return Err(unavailable("browser window is unavailable or minimized"));
        }
        let mut before = RECT::default();
        GetWindowRect(root, &mut before).map_err(|_| unavailable("native browser bounds unavailable"))?;
        let mut scan = Scan { root, visited: 0, exhausted: false, candidates: Vec::new() };
        let _ = EnumChildWindows(root, Some(child), LPARAM(&mut scan as *mut Scan as isize));
        if scan.exhausted || scan.candidates.len() != 1 {
            return Err(unavailable("native browser viewport is unavailable or ambiguous"));
        }
        let (widget, viewport) = scan.candidates[0];
        let mut after = RECT::default();
        GetWindowRect(root, &mut after).map_err(|_| unavailable("native browser bounds unavailable"))?;
        if !owned(root, pid) || GetAncestor(widget, GA_ROOT) != root || before != after
            || !IsWindowVisible(widget).as_bool() || client_bounds(widget) != Some(viewport) {
            return Err(unavailable("native browser viewport changed during measurement"));
        }
        if viewport.x < before.left as f64 || viewport.y < before.top as f64
            || viewport.x + viewport.width > after.right as f64
            || viewport.y + viewport.height > after.bottom as f64 {
            return Err(unavailable("native browser viewport is outside its owning window"));
        }
        Ok(NativeBrowserViewport {
            window: Rect { x: before.left as f64, y: before.top as f64,
                width: (before.right - before.left) as f64,
                height: (before.bottom - before.top) as f64 },
            viewport,
            source: "win32_render_widget_client",
        })
    }
}
