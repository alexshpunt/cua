//! Capture-bound native wheel input for one exact window. No desktop or DOM fallback.

use super::{
    bitmap_to_screen, capture_admission_refusal, exact_window_ownership_result, ToolState,
};
use async_trait::async_trait;
use cua_driver_contract::{
    PointerMoveDelivery, PointerMoveEffect, PointerMoveRoute, PointerScrollInput,
    PointerScrollOutput, ToolInput,
};
use cua_driver_core::protocol::ToolResult;
use cua_driver_core::tool::{Tool, ToolDef};
use cua_driver_core::tool_args::parse_typed_input;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub(super) struct ScrollAtTool {
    pub state: Arc<ToolState>,
}

fn refusal(code: &str, detail: impl Into<String>) -> ToolResult {
    let detail = detail.into();
    ToolResult::error(detail.clone()).with_structured(json!({
        "status":"refused", "code":code, "effect":"refused",
        "attempted":false, "delivered":false, "detail":detail,
        "pointer_attempted":false, "pointer_delivered":false,
        "attempted_axes":[], "delivered_axes":[],
    }))
}

#[async_trait]
impl Tool for ScrollAtTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("scroll_at").expect("native scroll contract"),
            )
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let input: PointerScrollInput = match parse_typed_input("scroll_at", args.clone()) {
            Ok(input) => input,
            Err(error) => return error,
        };
        if let Err(detail) = input.validate() {
            return refusal("invalid_arguments", detail);
        }
        if args["delivery_mode"] != "foreground" {
            return ToolResult::error("Native Windows coordinate scroll requires explicit foreground delivery after this refusal")
                .with_structured(json!({
                    "status":"refused", "code":"background_unavailable", "effect":"refused",
                    "attempted":false, "delivered":false, "activation_attempted":false,
                    "pointer_attempted":false, "pointer_delivered":false,
                    "attempted_axes":[], "delivered_axes":[],
                    "foreground_changed":false, "pointer_changed":false,
                    "delivery":{"mode":"background"},
                    "escalation":{"recommended":"foreground","reason":"Coordinate wheel input requires the system input queue"},
                }));
        }
        let state = self.state.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(cancelled.clone());
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let pid = input.pid;
            let hwnd = input.window_id;
            if let Err(error) = exact_window_ownership_result(pid, hwnd, crate::win32::window_owner_pid(hwnd)) {
                return error;
            }
            let geometry = match crate::input::pointer::PointerGeometry::read(hwnd, bitmap_to_screen) {
                Ok(geometry) => geometry,
                Err(error) => return refusal(error.code, error.detail),
            };
            let Some(bridge) = &state.capture_bridge else {
                return capture_admission_refusal(anyhow::anyhow!("capture service is unavailable"));
            };
            let point = match bridge.admit_click(
                &args,
                crate::capture_admission::WindowsCaptureTarget::Window { pid, window_id: hwnd },
                input.x,
                input.y,
            ) {
                Ok(Some(point)) => point,
                Ok(None) => return refusal("capture_action_refused", "Coordinate scroll requires a capture ID"),
                Err(error) => return capture_admission_refusal(error),
            };
            let point = match crate::capture_admission::round_action_point(point.0, point.1) {
                Ok(point) => point,
                Err(error) => return capture_admission_refusal(error),
            };
            let outcome = crate::input::pointer::send_scroll(
                hwnd, pid, point, (input.delta_x, input.delta_y), geometry, cancelled, bitmap_to_screen,
            );
            let failure = outcome.report.failure;
            let attempted = outcome.report.pointer_attempted || !outcome.report.attempted_axes.is_empty();
            let delivered = failure.is_none() && outcome.report.pointer_delivered;
            let output = PointerScrollOutput {
                attempted,
                delivered,
                pointer_attempted: outcome.report.pointer_attempted,
                pointer_delivered: outcome.report.pointer_delivered,
                delta_x: input.delta_x,
                delta_y: input.delta_y,
                unit: input.unit,
                attempted_axes: outcome.report.attempted_axes,
                delivered_axes: outcome.report.delivered_axes,
                route: PointerMoveRoute::Native,
                effect: PointerMoveEffect::Unverifiable,
                delivery_mode: PointerMoveDelivery::Foreground,
                foreground_changed: Some(outcome.foreground_before != outcome.foreground_after),
                pointer_changed: outcome.pointer_before.zip(outcome.pointer_after).map(|(before, after)| before != after),
                foreground_at_target: Some(outcome.foreground_after == hwnd),
                pointer_at_target: outcome.pointer_after.zip(outcome.pointer_target).map(|(position, target)| position == target),
            };
            let mut content = serde_json::to_value(output).expect("native scroll receipt is serializable");
            if let Some(error) = failure {
                content["status"] = json!(if attempted || outcome.activation_attempted { "unknown" } else { "refused" });
                content["activation_attempted"] = json!(outcome.activation_attempted);
                content["code"] = json!(error.code);
                content["detail"] = json!(error.detail);
                return ToolResult::error(error.detail).with_structured(content);
            }
            ToolResult::text("Native wheel input delivered; content displacement is unverified. Focus and pointer were not restored. Observe the exact window again.")
                .with_structured(content)
        }).await;
        match result {
            Ok(result) => result,
            Err(error) => ToolResult::error(error.to_string()).with_structured(json!({
                "status":"unknown", "code":"scroll_worker_failed", "effect":"unverifiable",
                "attempted":null, "delivered":null, "activation_attempted":null,
                "pointer_attempted":null, "pointer_delivered":null,
                "attempted_axes":null, "delivered_axes":null,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ScrollAtTool {
        ScrollAtTool {
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
    async fn background_refuses_before_native_identity_or_capture_lookup() {
        let result = tool()
            .invoke(json!({
                "pid":42,"window_id":7,"capture_id":"not-a-live-capture",
                "x":1,"y":2,"delta_y":1,"unit":"wheel_detents"
            }))
            .await;
        let content = result.structured_content.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(content["code"], "background_unavailable");
        assert_eq!(content["attempted"], false);
        assert_eq!(content["activation_attempted"], false);
        assert_eq!(content["pointer_attempted"], false);
        assert_eq!(content["attempted_axes"], json!([]));
        assert_eq!(content["foreground_changed"], false);
        assert_eq!(content["pointer_changed"], false);
    }
}
