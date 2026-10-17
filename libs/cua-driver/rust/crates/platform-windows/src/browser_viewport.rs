//! Readonly physical RootWebArea measurement. No HWND/chrome-inset fallback.
use cua_driver_core::browser::{
    dom_geometry::NativeBrowserViewport,
    refusal::{BrowserRefusal, BrowserRefusalCode},
    types::Rect,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows::core::Interface;
use windows::Win32::{
    Foundation::{E_POINTER, HWND, RECT},
    System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED, SAFEARRAY,
        },
        Ole::{
            SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetLBound,
            SafeArrayGetUBound, SafeArrayGetVartype,
        },
        Variant::VT_I4,
    },
    UI::{
        Accessibility::{
            CUIAutomation8, IUIAutomation2, IUIAutomationElement, IUIAutomationTreeWalker,
            UIA_DocumentControlTypeId,
        },
        HiDpi::{
            AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        WindowsAndMessaging::{
            GetAncestor, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
            IsWindowVisible, GA_ROOT,
        },
    },
};

fn unavailable(reason: &str) -> BrowserRefusal {
    BrowserRefusal::new(BrowserRefusalCode::BrowserRouteUnavailable, reason)
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct PhysicalPixels(DPI_AWARENESS_CONTEXT);
impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}
struct Array(*mut SAFEARRAY);
impl Drop for Array {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                let _ = SafeArrayDestroy(self.0);
            }
        }
    }
}

unsafe fn runtime_id(element: &IUIAutomationElement) -> windows::core::Result<Vec<i32>> {
    let array = Array(element.GetRuntimeId()?);
    if array.0.is_null() || SafeArrayGetDim(array.0) != 1 || SafeArrayGetVartype(array.0)? != VT_I4
    {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let first = SafeArrayGetLBound(array.0, 1)?;
    let last = SafeArrayGetUBound(array.0, 1)?;
    if !(1..=64).contains(&(i64::from(last) - i64::from(first) + 1)) {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let mut result = Vec::new();
    for index in first..=last {
        let mut value = 0i32;
        SafeArrayGetElement(array.0, &index, &mut value as *mut _ as *mut _)?;
        result.push(value);
    }
    Ok(result)
}

// Nullable output is not an HRESULT: real provider failures must remain failures.
unsafe fn relative(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    child: bool,
) -> windows::core::Result<Option<IUIAutomationElement>> {
    let mut output = std::ptr::null_mut();
    let method = if child {
        walker.vtable().GetFirstChildElement
    } else {
        walker.vtable().GetNextSiblingElement
    };
    method(walker.as_raw(), element.as_raw(), &mut output).ok()?;
    Ok((!output.is_null()).then(|| IUIAutomationElement::from_raw(output)))
}

fn rect(bounds: RECT) -> Rect {
    Rect {
        x: bounds.left as f64,
        y: bounds.top as f64,
        width: (i64::from(bounds.right) - i64::from(bounds.left)) as f64,
        height: (i64::from(bounds.bottom) - i64::from(bounds.top)) as f64,
    }
}

unsafe fn window_sample(root: HWND, pid: u32) -> windows::core::Result<(u32, RECT)> {
    let mut owner = 0;
    let thread = GetWindowThreadProcessId(root, Some(&mut owner));
    if !IsWindow(root).as_bool()
        || owner != pid
        || thread == 0
        || GetAncestor(root, GA_ROOT) != root
        || !IsWindowVisible(root).as_bool()
        || IsIconic(root).as_bool()
    {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let mut bounds = RECT::default();
    GetWindowRect(root, &mut bounds)?;
    Ok((thread, bounds))
}

#[derive(Debug, PartialEq)]
struct RootSample {
    bounds: RECT,
    identity: Vec<i32>,
}
unsafe fn root_sample(
    element: &IUIAutomationElement,
    pid: u32,
) -> windows::core::Result<RootSample> {
    if element.CurrentControlType()? != UIA_DocumentControlTypeId
        || element.CurrentAutomationId()?.to_string() != "RootWebArea"
        || element.CurrentFrameworkId()?.to_string() != "Chrome"
        || element.CurrentProcessId()? != pid as i32
        || element.CurrentIsOffscreen()?.as_bool()
    {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let bounds = element.CurrentBoundingRectangle()?;
    Ok(RootSample {
        bounds,
        identity: runtime_id(element)?,
    })
}

/// Complete traversal is required before `finish`. All top-level candidates
/// count, including not-yet-ready ones; zero roots never become a guessed origin.
#[derive(Default)]
struct RootSelection {
    visited: usize,
    samples: Vec<RootSample>,
}
impl RootSelection {
    fn visit(&mut self, depth: usize) -> windows::core::Result<()> {
        if depth > 24 || self.visited >= 512 {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        self.visited += 1;
        Ok(())
    }
    fn add(&mut self, sample: RootSample) -> windows::core::Result<()> {
        if !self.samples.is_empty()
            || sample.bounds.right <= sample.bounds.left
            || sample.bounds.bottom <= sample.bounds.top
            || sample.identity.is_empty()
            || sample.identity.len() > 64
        {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        self.samples.push(sample);
        Ok(())
    }
    fn finish(mut self) -> windows::core::Result<RootSample> {
        self.samples
            .pop()
            .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
    }
}
struct Scan<'a> {
    pid: u32,
    selection: RootSelection,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    root: Option<IUIAutomationElement>,
}
impl Scan<'_> {
    unsafe fn walk(
        &mut self,
        walker: &IUIAutomationTreeWalker,
        element: &IUIAutomationElement,
        depth: usize,
    ) -> windows::core::Result<()> {
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        self.selection.visit(depth)?;
        let control = element.CurrentControlType()?;
        // Traverse raw ancestry, not actionable-node projection. Once an actual
        // RootWebArea is found, descendant frame roots cannot be top-level roots.
        if control == UIA_DocumentControlTypeId
            && element.CurrentAutomationId()?.to_string() == "RootWebArea"
        {
            let sample = root_sample(element, self.pid)?;
            self.selection.add(sample)?;
            self.root = Some(element.clone());
            return Ok(());
        }
        let mut next = relative(walker, element, true)?;
        while let Some(child) = next {
            self.walk(walker, &child, depth + 1)?;
            next = relative(walker, &child, false)?;
        }
        Ok(())
    }
}

/// Measure only a unique, ready Chromium RootWebArea beneath the exact HWND.
/// Core must independently bracket this with selected-tab/document validation.
pub(crate) fn measure(pid: i64, window: u64) -> Result<NativeBrowserViewport, BrowserRefusal> {
    let pid = u32::try_from(pid).map_err(|_| unavailable("invalid browser owner"))?;
    crate::uia::windows_enum::run_readonly_with_deadline(
        "browser-viewport",
        Duration::from_secs(2),
        move |cancelled| unsafe { measure_inner(pid, HWND(window as *mut _), &cancelled) },
    )
    .map_err(unavailable)?
    .map_err(|_| unavailable("native RootWebArea is pending, changed, ambiguous or incomplete"))
}

unsafe fn measure_inner(
    pid: u32,
    root: HWND,
    cancelled: &AtomicBool,
) -> windows::core::Result<NativeBrowserViewport> {
    let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    if previous.0.is_null() {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let _physical = PhysicalPixels(previous);
    if !AreDpiAwarenessContextsEqual(
        GetThreadDpiAwarenessContext(),
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    )
    .as_bool()
    {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    let _com = Com;
    let before = window_sample(root, pid)?;
    let automation: IUIAutomation2 = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
    automation.SetConnectionTimeout(200)?;
    automation.SetTransactionTimeout(200)?;
    let element = automation.ElementFromHandle(root)?;
    if element.CurrentNativeWindowHandle()? != root || element.CurrentProcessId()? != pid as i32 {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let walker = automation.RawViewWalker()?;
    let mut scan = Scan {
        pid,
        selection: RootSelection::default(),
        deadline: Instant::now() + Duration::from_millis(1500),
        cancelled,
        root: None,
    };
    scan.walk(&walker, &element, 0)?;
    if cancelled.load(Ordering::Acquire) || Instant::now() >= scan.deadline {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let sample = scan.selection.finish()?;
    let element = scan
        .root
        .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
    if root_sample(&element, pid)? != sample || window_sample(root, pid)? != before {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    let window = rect(before.1);
    let viewport = rect(sample.bounds);
    if viewport.x < window.x
        || viewport.y < window.y
        || viewport.x + viewport.width > window.x + window.width
        || viewport.y + viewport.height > window.y + window.height
    {
        return Err(windows::core::Error::from_hresult(E_POINTER));
    }
    Ok(NativeBrowserViewport {
        window,
        viewport,
        source: "uia_root_web_area",
        identity: sample.identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> RootSample {
        RootSample {
            bounds: RECT {
                left: 71,
                top: 191,
                right: 1400,
                bottom: 1010,
            },
            identity: vec![42, 1, 7],
        }
    }
    #[test]
    fn root_selection_requires_one_ready_native_document() {
        assert_eq!(
            RootSelection::default().finish().unwrap_err().code(),
            E_POINTER
        );
        let mut selection = RootSelection::default();
        let mut zero = root();
        zero.bounds.bottom = zero.bounds.top;
        assert_eq!(selection.add(zero).unwrap_err().code(), E_POINTER);
        selection.add(root()).unwrap();
        assert_eq!(selection.add(root()).unwrap_err().code(), E_POINTER);
    }
    #[test]
    fn incomplete_raw_traversal_is_not_successful_candidate_selection() {
        let mut selection = RootSelection::default();
        selection.add(root()).unwrap();
        for _ in 0..512 {
            selection.visit(0).unwrap();
        }
        assert_eq!(selection.visit(0).unwrap_err().code(), E_POINTER);
        assert_eq!(
            RootSelection::default().visit(25).unwrap_err().code(),
            E_POINTER
        );
    }
}
