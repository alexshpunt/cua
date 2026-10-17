//! Direct native samples only: no captures, browser/CDP calls, activation or input.
use super::{
    candidate_state, legacy_candidates, Scan, WindowRow, HWND_LIMIT, UIA_DEPTH_LIMIT, UIA_LIMIT,
};
use serde_json::{json, Value};
use std::time::Instant;
use windows::core::Interface;
use windows::Win32::{
    Foundation::{BOOL, E_POINTER, FALSE, HWND, LPARAM, POINT, RECT, TRUE},
    Graphics::Gdi::ClientToScreen,
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
            AreDpiAwarenessContextsEqual, GetDpiForWindow, GetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        WindowsAndMessaging::{
            EnumChildWindows, GetAncestor, GetClassNameW, GetClientRect, GetParent, GetWindowRect,
            GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GA_ROOT,
        },
    },
};

fn rect(value: RECT) -> Value {
    json!({"left": value.left, "top": value.top, "right": value.right, "bottom": value.bottom})
}

fn sample<T: serde::Serialize>(result: windows::core::Result<T>) -> Value {
    match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error.to_string()}),
    }
}

fn text(result: windows::core::Result<windows::core::BSTR>) -> Value {
    match result {
        Ok(value) => {
            let value = value.to_string();
            json!({"value": value.chars().take(4096).collect::<String>(), "truncated": value.chars().count() > 4096})
        }
        Err(error) => json!({"error": error.to_string()}),
    }
}

unsafe fn window_rect(hwnd: HWND) -> windows::core::Result<RECT> {
    let mut bounds = RECT::default();
    GetWindowRect(hwnd, &mut bounds)?;
    Ok(bounds)
}

unsafe fn client_rects(hwnd: HWND) -> Value {
    let mut bounds = RECT::default();
    if let Err(error) = GetClientRect(hwnd, &mut bounds) {
        return json!({"error": error.to_string()});
    }
    let mut first = POINT {
        x: bounds.left,
        y: bounds.top,
    };
    let mut last = POINT {
        x: bounds.right,
        y: bounds.bottom,
    };
    let mapped =
        ClientToScreen(hwnd, &mut first).as_bool() && ClientToScreen(hwnd, &mut last).as_bool();
    json!({"client_rect": rect(bounds), "screen_rect": mapped.then(|| json!({"left": first.x, "top": first.y, "right": last.x, "bottom": last.y}))})
}

unsafe fn root_sample(pid: u32, root: HWND) -> Result<Value, String> {
    let mut owner = 0;
    let thread = GetWindowThreadProcessId(root, Some(&mut owner));
    if !IsWindow(root).as_bool()
        || owner != pid
        || thread == 0
        || GetAncestor(root, GA_ROOT) != root
    {
        return Err("exact root HWND/PID ownership is unavailable".into());
    }
    let bounds = window_rect(root).map_err(|error| error.to_string())?;
    Ok(
        json!({"hwnd": root.0 as u64, "pid": owner, "thread_id": thread,
        "window_rect": rect(bounds), "client": client_rects(root), "dpi": GetDpiForWindow(root),
        "visible": IsWindowVisible(root).as_bool(), "minimized": IsIconic(root).as_bool()}),
    )
}

struct HwndScan {
    root: HWND,
    deadline: Instant,
    scan: Scan<WindowRow<Value>>,
}

unsafe extern "system" fn child(hwnd: HWND, data: LPARAM) -> BOOL {
    let state = &mut *(data.0 as *mut HwndScan);
    if Instant::now() >= state.deadline {
        state.scan.stop = Some("deadline");
        return FALSE;
    }
    if !state.scan.admit() {
        return FALSE;
    }
    if GetAncestor(hwnd, GA_ROOT) != state.root {
        state.scan.stop = Some("descendant_root_changed");
        return FALSE;
    }
    let visible = IsWindowVisible(hwnd).as_bool();
    let mut name = [0u16; 256];
    let length = GetClassNameW(hwnd, &mut name);
    let class_name = (length > 0 && (length as usize) < name.len() - 1)
        .then(|| String::from_utf16_lossy(&name[..length as usize]));
    let mut pid = 0;
    let thread = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let bounds = match window_rect(hwnd) {
        Ok(bounds) => json!({"value": rect(bounds)}),
        Err(error) => json!({"error": error.to_string()}),
    };
    state.scan.record(WindowRow { visible, class_name, data: json!({"hwnd": hwnd.0 as u64,
        "pid": pid, "thread_id": thread, "parent": GetParent(hwnd).ok().map(|parent| parent.0 as u64),
        "window_rect": bounds, "client": client_rects(hwnd)}) });
    TRUE
}

struct Array(*mut SAFEARRAY);
impl Drop for Array {
    fn drop(&mut self) {
        unsafe {
            let _ = SafeArrayDestroy(self.0);
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

unsafe fn document(element: &IUIAutomationElement, depth: usize) -> Value {
    let bounds = match element.CurrentBoundingRectangle() {
        Ok(bounds) => json!({"value": rect(bounds)}),
        Err(error) => json!({"error": error.to_string()}),
    };
    json!({"depth": depth, "bounds": bounds, "runtime_id": sample(runtime_id(element)),
        "offscreen": sample(element.CurrentIsOffscreen().map(|value| value.as_bool())),
        "provider": text(element.CurrentProviderDescription()), "framework": text(element.CurrentFrameworkId()),
        "name": text(element.CurrentName()), "automation_id": text(element.CurrentAutomationId()),
        "class_name": text(element.CurrentClassName()), "pid": sample(element.CurrentProcessId()),
        "native_hwnd": sample(element.CurrentNativeWindowHandle().map(|value| value.0 as u64))})
}

// Inspect the raw HRESULT separately from its nullable out pointer: a genuine
// provider E_POINTER must not be misclassified as an empty child/sibling list.
unsafe fn relative_element(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    first_child: bool,
) -> Result<Option<IUIAutomationElement>, String> {
    let mut result = std::ptr::null_mut();
    let method = if first_child {
        walker.vtable().GetFirstChildElement
    } else {
        walker.vtable().GetNextSiblingElement
    };
    method(walker.as_raw(), element.as_raw(), &mut result)
        .ok()
        .map_err(|error| error.to_string())?;
    Ok((!result.is_null()).then(|| IUIAutomationElement::from_raw(result)))
}

unsafe fn walk(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    depth: usize,
    deadline: Instant,
    scan: &mut Scan<Value>,
) -> Result<(), String> {
    if Instant::now() >= deadline {
        scan.stop = Some("deadline");
        return Ok(());
    }
    if depth > UIA_DEPTH_LIMIT {
        scan.stop = Some("depth_budget");
        return Ok(());
    }
    if !scan.admit() {
        return Ok(());
    }
    let control = element
        .CurrentControlType()
        .map_err(|error| error.to_string())?;
    scan.record(if control == UIA_DocumentControlTypeId {
        document(element, depth)
    } else {
        Value::Null
    });
    let mut next = relative_element(walker, element, true)?;
    while let Some(child) = next {
        walk(walker, &child, depth + 1, deadline, scan)?;
        if scan.stop.is_some() {
            break;
        }
        next = relative_element(walker, &child, false)?;
    }
    Ok(())
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

unsafe fn uia(pid: u32, root: HWND, deadline: Instant) -> Value {
    let mut scan = Scan::new(UIA_LIMIT);
    let result = (|| -> Result<(), String> {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|error| error.to_string())?;
        let _com = Com;
        let automation: IUIAutomation2 =
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| error.to_string())?;
        automation
            .SetConnectionTimeout(200)
            .map_err(|error| error.to_string())?;
        automation
            .SetTransactionTimeout(200)
            .map_err(|error| error.to_string())?;
        let element = automation
            .ElementFromHandle(root)
            .map_err(|error| error.to_string())?;
        if element
            .CurrentNativeWindowHandle()
            .map_err(|error| error.to_string())?
            != root
            || element
                .CurrentProcessId()
                .map_err(|error| error.to_string())?
                != pid as i32
        {
            return Err("UIA root HWND/PID changed".into());
        }
        let walker = automation
            .RawViewWalker()
            .map_err(|error| error.to_string())?;
        walk(&walker, &element, 0, deadline, &mut scan)
    })();
    if result.is_err() {
        scan.stop = Some("native_error");
    }
    let documents: Vec<_> = scan.rows.iter().filter(|row| !row.is_null()).collect();
    json!({"tree_view": "raw", "visited": scan.rows.len(), "complete": scan.stop.is_none(),
        "stop_reason": scan.stop, "error": result.err(), "candidate_state": candidate_state(documents.len(), scan.stop.is_none()),
        "documents": documents})
}

/// Diagnose only the explicitly addressed native window. This does not validate a
/// selected tab/document or exclude same-process HWND reuse; no browser authority is minted.
pub fn inspect(pid: u32, window: u64, deadline: Instant) -> Result<Value, String> {
    unsafe {
        if !AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .as_bool()
        {
            return Err("physical per-monitor-v2 thread coordinates are required".into());
        }
        let root = HWND(window as *mut _);
        let before = root_sample(pid, root)?;
        let mut state = HwndScan {
            root,
            deadline,
            scan: Scan::new(HWND_LIMIT),
        };
        // EnumChildWindows documents its return value as unused. Completion is
        // callback-budget state plus separately sampled root ownership, not BOOL.
        let _ = EnumChildWindows(root, Some(child), LPARAM(&mut state as *mut _ as isize));
        let (count, classification) = legacy_candidates(&state.scan);
        let children: Vec<_> = state.scan.rows.iter().map(|row| json!({"visible": row.visible,
            "class_name": row.class_name, "class_complete": row.class_name.is_some(), "sample": row.data})).collect();
        let uia = uia(pid, root, deadline);
        let after = root_sample(pid, root);
        let equal = after.as_ref().ok().map(|sample| *sample == before);
        let after = match after {
            Ok(value) => value,
            Err(error) => json!({"error": error}),
        };
        Ok(
            json!({"format": "native_viewport_diagnostic_v1", "readonly": true,
            "coordinate_space": "windows_desktop_pixels", "atomic": false, "native_mapping_qualified": false,
            "selected_tab_proven": false, "document_identity_proven": false, "window_lifetime_proven": false,
            "root_before": before, "root_after": after, "root_samples_equal": equal,
            "hwnd_scan": {"complete": state.scan.stop.is_none(), "stop_reason": state.scan.stop,
                "visited": state.scan.rows.len(), "legacy_candidate_count": count, "candidate_state": classification, "children": children},
            "uia_scan": uia}),
        )
    }
}
