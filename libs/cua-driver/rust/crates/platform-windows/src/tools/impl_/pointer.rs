//! Exact-window native pointer input. No desktop, button or key fallback.

use super::{
    bitmap_to_screen, capture_admission_refusal, exact_window_ownership_result, ToolState,
};
use async_trait::async_trait;
use cua_driver_contract::{
    PointerMoveDelivery, PointerMoveEffect, PointerMoveInput, PointerMoveOutput, PointerMoveRoute,
    ToolInput,
};
use cua_driver_core::protocol::ToolResult;
use cua_driver_core::tool::{Tool, ToolDef};
use cua_driver_core::tool_args::parse_typed_input;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub(super) struct MovePointerTool {
    pub state: Arc<ToolState>,
}

fn refusal(code: &str, detail: impl Into<String>) -> ToolResult {
    let detail = detail.into();
    ToolResult::error(detail.clone()).with_structured(json!({
        "status":"refused", "code":code, "effect":"refused",
        "attempted":false, "delivered":false, "detail":detail,
    }))
}

fn token_point(
    element: &crate::uia::snapshot::RetainedElement,
    pid: u32,
    geometry: &crate::input::pointer::PointerGeometry,
) -> Result<(i32, i32), ToolResult> {
    use std::mem::ManuallyDrop;
    use windows::core::Interface;
    use windows::Win32::UI::Accessibility::IUIAutomationElement;

    if !element.is_uia() {
        return Err(refusal(
            "element_geometry_unavailable",
            "Native pointer tokens currently require UIA geometry",
        ));
    }
    // The snapshot retains the COM reference; this wrapper must not release it.
    let native =
        ManuallyDrop::new(unsafe { IUIAutomationElement::from_raw(element.as_ptr() as *mut _) });
    let query_error =
        |error: windows::core::Error| refusal("element_unavailable", error.to_string());
    if unsafe { native.CurrentProcessId() }.map_err(query_error)? != pid as i32 {
        return Err(refusal(
            "element_target_mismatch",
            "Element no longer belongs to the admitted process",
        ));
    }
    if !unsafe { native.CurrentIsEnabled() }
        .map_err(query_error)?
        .as_bool()
    {
        return Err(refusal(
            "element_disabled",
            "The target element is disabled",
        ));
    }
    if unsafe { native.CurrentIsOffscreen() }
        .map_err(query_error)?
        .as_bool()
    {
        return Err(refusal(
            "element_not_visible",
            "The target element is offscreen; no scroll was attempted",
        ));
    }
    let current = unsafe { native.CurrentBoundingRectangle() }.map_err(query_error)?;
    let rect = (current.left, current.top, current.right, current.bottom);
    if element.rect != Some(rect) || current.right <= current.left || current.bottom <= current.top
    {
        return Err(refusal(
            "element_geometry_changed",
            "Element bounds changed; observe the exact window again",
        ));
    }
    geometry
        .local_point(element.center)
        .map_err(|error| refusal(error.code, error.detail))
}

#[async_trait]
impl Tool for MovePointerTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("move_pointer")
                    .expect("native pointer contract"),
            )
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let input: PointerMoveInput = match parse_typed_input("move_pointer", args.clone()) {
            Ok(input) => input,
            Err(error) => return error,
        };
        if let Err(detail) = input.validate() {
            return refusal("invalid_arguments", detail);
        }
        if args["delivery_mode"] != "foreground" {
            return refusal("background_unavailable", "Real Windows pointer motion requires explicit foreground delivery after this refusal")
                .with_structured(json!({
                    "status":"refused", "code":"background_unavailable", "effect":"refused",
                    "attempted":false, "delivered":false, "activation_attempted":false,
                    "foreground_changed":false, "pointer_changed":false,
                    "delivery":{"mode":"background"},
                    "escalation":{"recommended":"foreground","reason":"Real pointer hover requires the system input queue"},
                }));
        }
        let pid = input.pid;
        let hwnd = input.window_id;
        let dwell_ms = args["dwell_ms"].as_u64().unwrap_or(0);
        let admitted = if args.get("element_token").is_some() {
            let resolved = match self.state.snapshots.resolve(pid as i32, &args) {
                Ok(resolved) => resolved,
                Err(error) => return error,
            };
            let (_, window_id, admitted) = resolved.into_parts(None);
            if window_id != Some(hwnd) {
                return refusal(
                    "conflicting_element_target",
                    "Token belongs to a different exact window",
                );
            }
            if admitted.is_none() {
                return refusal("invalid_element_token", "No admitted element was retained");
            }
            admitted
        } else {
            None
        };
        let state = self.state.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(cancelled.clone());
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            if let Err(error) = exact_window_ownership_result(pid, hwnd, crate::win32::window_owner_pid(hwnd)) {
                return error;
            }
            let geometry = match crate::input::pointer::PointerGeometry::read(hwnd, bitmap_to_screen) {
                Ok(geometry) => geometry,
                Err(error) => return refusal(error.code, error.detail),
            };
            let local_point = if let Some(element) = &admitted {
                use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
                let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                if hr.is_err() && hr.0 != -2147417850 {
                    return refusal("element_unavailable", hr.to_string());
                }
                struct Apartment(bool);
                impl Drop for Apartment {
                    fn drop(&mut self) {
                        if self.0 { unsafe { CoUninitialize(); } }
                    }
                }
                let _apartment = Apartment(hr.is_ok());
                match token_point(element, pid, &geometry) {
                    Ok(point) => point,
                    Err(error) => return error,
                }
            } else {
                let Some(bridge) = &state.capture_bridge else {
                    return capture_admission_refusal(anyhow::anyhow!("capture service is unavailable"));
                };
                let point = match bridge.admit_click(
                    &args,
                    crate::capture_admission::WindowsCaptureTarget::Window { pid, window_id: hwnd },
                    args["x"].as_f64().expect("validated pointer X"),
                    args["y"].as_f64().expect("validated pointer Y"),
                ) {
                    Ok(Some(point)) => point,
                    Ok(None) => return refusal("capture_action_refused", "Pointer pixels require a capture ID"),
                    Err(error) => return capture_admission_refusal(error),
                };
                match crate::capture_admission::round_action_point(point.0, point.1) {
                    Ok(point) => point,
                    Err(error) => return capture_admission_refusal(error),
                }
            };
            let outcome = crate::input::pointer::send_motion(
                hwnd, pid, local_point, dwell_ms, geometry, cancelled, bitmap_to_screen,
            );
            let foreground_changed = outcome.foreground_before != outcome.foreground_after;
            let pointer_changed = outcome.pointer_before.zip(outcome.pointer_after)
                .map(|(before, after)| before != after);
            let pointer_at_target = outcome.pointer_after.zip(outcome.pointer_target)
                .map(|(position, target)| position == target);
            let output = PointerMoveOutput {
                attempted: outcome.report.attempted,
                delivered: outcome.report.delivered,
                route: PointerMoveRoute::Native,
                effect: PointerMoveEffect::Unverifiable,
                delivery_mode: PointerMoveDelivery::Foreground,
                dwell_ms,
                dwell_elapsed_ms: outcome.report.dwell_elapsed_ms,
                foreground_changed: Some(foreground_changed),
                pointer_changed,
                foreground_at_target: Some(outcome.foreground_after == hwnd),
                pointer_at_target,
            };
            let mut content = serde_json::to_value(output).expect("native pointer receipt is serializable");
            if let Some(error) = outcome.report.failure {
                content["status"] = json!(if outcome.report.attempted || outcome.activation_attempted { "unknown" } else { "refused" });
                content["activation_attempted"] = json!(outcome.activation_attempted);
                content["code"] = json!(error.code);
                content["detail"] = json!(error.detail);
                return ToolResult::error(error.detail).with_structured(content);
            }
            ToolResult::text("Native pointer movement delivered; hover/app effects are unverified. Focus and pointer were not restored. Observe the exact window again.")
                .with_structured(content)
        }).await;
        match result {
            Ok(result) => result,
            Err(error) => ToolResult::error(error.to_string()).with_structured(json!({
                "status":"unknown", "code":"pointer_worker_failed", "effect":"unverifiable",
                "attempted":null, "delivered":null, "activation_attempted":null,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> MovePointerTool {
        MovePointerTool {
            state: Arc::new(ToolState {
                snapshots: Arc::new(super::super::Snapshots::new()),
                cursor_registry: Arc::new(super::super::CursorRegistry::new()),
                config: Arc::new(std::sync::RwLock::new(super::super::DriverConfig::default())),
                capture_bridge: None,
                persistent_wgc: Arc::new(crate::persistent_wgc::Manager::new()),
            }),
        }
    }

    #[tokio::test]
    async fn background_refuses_before_native_window_or_capture_lookup() {
        let result = tool().invoke(json!({"pid":42,"window_id":7,"capture_id":"not-a-live-capture","x":1,"y":2,"dwell_ms":100})).await;
        let content = result.structured_content.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(content["code"], "background_unavailable");
        assert_eq!(content["attempted"], false);
        assert_eq!(content["activation_attempted"], false);
        assert_eq!(content["foreground_changed"], false);
        assert_eq!(content["pointer_changed"], false);
    }

    #[tokio::test]
    async fn stale_foreground_token_refuses_before_native_work() {
        let result = tool().invoke(json!({"pid":42,"window_id":7,"element_token":"sffffffff:0","delivery_mode":"foreground"})).await;
        assert_eq!(result.is_error, Some(true));
        let content = result.structured_content.unwrap();
        assert_eq!(content["refusal"]["code"], "stale_element_token");
    }
}
