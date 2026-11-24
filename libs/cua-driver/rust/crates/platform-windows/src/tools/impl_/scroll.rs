// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Capture-bound native wheel input for one exact window. No desktop or DOM fallback.

use super::{
    bitmap_to_screen, capture_admission_refusal, exact_window_ownership_result, ToolState,
};
use async_trait::async_trait;
use cua_driver_contract::{
    PointerMoveEffect, PointerScrollInput, PointerScrollOutput, ScrollDelivery, ScrollRoute,
    ToolInput,
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

/// Wheel messages posted to the window at an admitted point: no pointer motion, no
/// activation. Surfaces that read the wheel only from the system queue refuse first.
async fn background_scroll(
    state: Arc<ToolState>,
    input: PointerScrollInput,
    args: Value,
) -> ToolResult {
    use crate::input::delivery::EventKind;
    let result = tokio::task::spawn_blocking(move || {
        let pid = input.pid;
        let hwnd = input.window_id;
        if let Err(error) =
            exact_window_ownership_result(pid, hwnd, crate::win32::window_owner_pid(hwnd))
        {
            return error;
        }
        if crate::input::delivery::would_be_silently_dropped(hwnd, EventKind::MouseWheel) {
            return crate::input::delivery::background_unavailable_error(
                hwnd,
                EventKind::MouseWheel,
            );
        }
        let Some(bridge) = &state.capture_bridge else {
            return capture_admission_refusal(anyhow::anyhow!("capture service is unavailable"));
        };
        let point = match bridge.admit_click(
            &args,
            crate::capture_admission::WindowsCaptureTarget::Window {
                pid,
                window_id: hwnd,
            },
            input.x,
            input.y,
        ) {
            Ok(Some(point)) => point,
            Ok(None) => {
                return refusal(
                    "capture_action_refused",
                    "Coordinate scroll requires a capture ID",
                )
            }
            Err(error) => return capture_admission_refusal(error),
        };
        let (x, y) = match crate::capture_admission::round_action_point(point.0, point.1) {
            Ok(point) => point,
            Err(error) => return capture_admission_refusal(error),
        };
        let (sx, sy) = bitmap_to_screen(hwnd, x, y);
        let foreground = || unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as usize as u64
        };
        let foreground_before = foreground();
        if let Err(error) =
            crate::input::mouse::post_wheel_screen(hwnd, sx, sy, input.delta_x, input.delta_y)
        {
            let message = error.to_string();
            return if message.starts_with(crate::input::mouse::POINTER_SURFACE_UNAVAILABLE) {
                crate::input::delivery::background_unavailable_error_with_cause(
                    hwnd,
                    EventKind::MouseWheel,
                    message,
                )
            } else {
                ToolResult::error(message)
            };
        }
        let foreground_after = foreground();
        let axes: Vec<_> = [
            (cua_driver_contract::ScrollAxis::Y, input.delta_y),
            (cua_driver_contract::ScrollAxis::X, input.delta_x),
        ]
        .into_iter()
        .filter_map(|(axis, delta)| (delta != 0).then_some(axis))
        .collect();
        let output = PointerScrollOutput {
            attempted: true,
            delivered: true,
            pointer_attempted: false,
            pointer_delivered: false,
            delta_x: input.delta_x,
            delta_y: input.delta_y,
            unit: input.unit,
            attempted_axes: axes.clone(),
            delivered_axes: axes,
            route: ScrollRoute::Posted,
            effect: PointerMoveEffect::Unverifiable,
            delivery_mode: ScrollDelivery::Background,
            foreground_changed: Some(foreground_before != foreground_after),
            pointer_changed: Some(false),
            foreground_at_target: Some(foreground_after == hwnd),
            pointer_at_target: None,
        };
        let content = serde_json::to_value(output).expect("native scroll receipt is serializable");
        ToolResult::text(
            "Wheel messages posted to the window in the background; the pointer did not move. \
             Content displacement is unverified: observe the window again.",
        )
        .with_structured(content)
    })
    .await;
    result.unwrap_or_else(|error| ToolResult::error(format!("scroll worker failed: {error}")))
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
            return background_scroll(self.state.clone(), input, args).await;
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
                route: ScrollRoute::Native,
                effect: PointerMoveEffect::Unverifiable,
                delivery_mode: ScrollDelivery::Foreground,
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
                window_status: Arc::new(crate::tools::impl_::window_restore::Manager::new()),
            }),
        }
    }

    #[tokio::test]
    async fn background_checks_the_exact_window_before_capture_lookup() {
        let result = tool()
            .invoke(json!({
                "pid":42,"window_id":7,"capture_id":"not-a-live-capture",
                "x":1,"y":2,"delta_y":1,"unit":"wheel_detents"
            }))
            .await;
        let content = result.structured_content.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(content["code"], "window_target_not_found");
        assert_eq!(content["effect"], "refused");
    }

    #[test]
    fn background_receipt_posts_without_moving_the_pointer() {
        use cua_driver_contract::{ScrollAxis, ScrollUnit, ToolOutput};
        let mut output = PointerScrollOutput {
            attempted: true,
            delivered: true,
            pointer_attempted: false,
            pointer_delivered: false,
            delta_x: 0,
            delta_y: 3,
            unit: ScrollUnit::WheelDetents,
            attempted_axes: vec![ScrollAxis::Y],
            delivered_axes: vec![ScrollAxis::Y],
            route: ScrollRoute::Posted,
            effect: PointerMoveEffect::Unverifiable,
            delivery_mode: ScrollDelivery::Background,
            foreground_changed: Some(false),
            pointer_changed: Some(false),
            foreground_at_target: Some(false),
            pointer_at_target: None,
        };
        assert_eq!(output.validate(), Ok(()));
        output.pointer_attempted = true;
        assert!(output.validate().is_err());
        output.pointer_attempted = false;
        output.route = ScrollRoute::Native;
        assert!(output.validate().is_err());
    }
}
