// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

use async_trait::async_trait;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
};
use serde_json::{json, Value};
use std::sync::Arc;

use super::drag::path_refusal;
use super::impl_::{bitmap_to_screen, screenshot_scale, ToolState};

// ── input_timeline ────────────────────────────────────────────────────────────

pub struct InputTimelineTool {
    pub(crate) state: Arc<ToolState>,
}

static INPUT_TIMELINE_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for InputTimelineTool {
    fn def(&self) -> &ToolDef {
        INPUT_TIMELINE_DEF.get_or_init(|| ToolDef {
            name: "input_timeline".into(),
            description: "Windows only: run one timed input program against an exact foreground window: key down/up, left/right/middle/x1/x2 button down/up, absolute moves in window-local screenshot pixels, raw relative moves for pointer-locked apps, wheel detents, and pen contacts with pressure, tilt, eraser and barrel. Events run at their at_ms offsets (non-decreasing, up to 120000 ms, up to 5000 events). Everything still held is released at the end, on error, on target or foreground loss and on cancellation. Explicit foreground and a prior screenshot of the window are required. Never replay an uncertain prefix.".into(),
            input_schema: json!({"type":"object","required":["pid","window_id","events"],"properties":{
                "session":cua_driver_core::tool_schema::session_schema(),
                "pid":{"type":"integer","minimum":1,"description":"Process id of the target window."},
                "window_id":{"type":"integer","minimum":1,"description":"Native window id from list_windows; must stay the foreground window."},
                "events":{"type":"array","minItems":1,"maxItems":crate::timeline::MAX_EVENTS,"description":"Timed input events, ordered by non-decreasing at_ms.","items":{"type":"object","required":["at_ms","type"],"properties":{
                    "at_ms":{"type":"integer","minimum":0,"maximum":crate::timeline::MAX_DURATION_MS},
                    "type":{"type":"string","enum":["key_down","key_up","button_down","button_up","move","move_relative","wheel","pen_down","pen_move","pen_up"]},
                    "key":{"type":"string"},
                    "button":{"type":"string","enum":["left","right","middle","x1","x2"]},
                    "x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0},
                    "dx":{"type":"integer"},"dy":{"type":"integer"},
                    "pressure":{"type":"number","minimum":0,"maximum":1},
                    "tilt_x":{"type":"integer","minimum":-90,"maximum":90},"tilt_y":{"type":"integer","minimum":-90,"maximum":90},
                    "eraser":{"type":"boolean"},"barrel":{"type":"boolean"}
                },"additionalProperties":false}},
                "delivery_mode":crate::input::delivery::delivery_mode_schema()
            },"additionalProperties":false}),
            read_only:false, destructive:true, idempotent:false, open_world:true,
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let request = match crate::timeline::Request::parse(&args) {
            Ok(request) => request,
            Err(code) => return path_refusal(code, "Invalid input timeline; no input was sent."),
        };
        if crate::input::delivery::DeliveryMode::from_args(&args)
            != crate::input::delivery::DeliveryMode::Foreground
        {
            return path_refusal(
                "background_unavailable",
                "Input timelines need delivery_mode:foreground. No input was sent.",
            );
        }
        let (pid, hwnd) = (request.pid, request.window_id);
        let ratio = match screenshot_scale(&self.state, &args, pid, Some(hwnd)) {
            Ok(ratio) => ratio,
            Err(refusal) => return refusal,
        };
        let program = match request.into_program(ratio) {
            Ok(program) => program,
            Err(code) => return path_refusal(code, "Invalid scaled timeline; no input was sent."),
        };
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(cancelled.clone());
        // The worker keeps desktop input admission until it has released everything it holds.
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            crate::input::timeline::send_timeline(hwnd, pid, &program, cancelled, bitmap_to_screen)
        })
        .await;
        use cua_driver_core::action_record::{
            ActionEffect, ActionExecutionRecord, ActionTransport, ActualDelivery, RequestedDelivery,
        };
        match result {
            Ok(Ok(report)) => {
                let summary = format!(
                    "Input timeline: {}/{} events sent in {} ms; everything held released: {}. {}",
                    report.sent,
                    report.total,
                    report.elapsed_ms,
                    report.released,
                    report
                        .error
                        .as_deref()
                        .unwrap_or("Delivery is not proof of the app's result.")
                );
                if report.sent == 0 && report.error.is_some() {
                    let code = report
                        .error
                        .as_deref()
                        .unwrap()
                        .split(':')
                        .next()
                        .unwrap_or("input_failed")
                        .to_owned();
                    return ToolResult::error(summary).with_structured(
                        json!({"code": code, "effect": "refused", "timeline": report.to_json()}),
                    );
                }
                let mut record = ActionExecutionRecord::new(
                    if report.error.is_none() && report.released {
                        ActionEffect::Unverifiable
                    } else {
                        ActionEffect::Partial
                    },
                    ActionTransport::WindowsSendInput,
                    RequestedDelivery::Foreground,
                );
                record.actual_delivery = Some(ActualDelivery::Foreground);
                record.delivered_count = Some(report.sent as u32);
                ToolResult::text(summary)
                    .with_structured(json!({"timeline": report.to_json()}))
                    .with_action_record(record)
            }
            Ok(Err(error)) => {
                path_refusal(error.split(':').next().unwrap_or("input_failed"), &error)
            }
            Err(error) => {
                let mut record = ActionExecutionRecord::new(
                    ActionEffect::Unverifiable,
                    ActionTransport::WindowsSendInput,
                    RequestedDelivery::Foreground,
                );
                record.actual_delivery = Some(ActualDelivery::Unknown);
                ToolResult::error(format!("Input timeline worker failed; prefix and release unconfirmed. Do not replay: {error}")).with_action_record(record)
            }
        }
    }
}
