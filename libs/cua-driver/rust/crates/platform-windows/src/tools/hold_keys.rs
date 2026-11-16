// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

use async_trait::async_trait;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
};
use serde_json::{json, Value};
use std::sync::Arc;

use super::drag::{complete_held_input, path_refusal};
use super::impl_::{bitmap_to_screen, resolve_cursor_key, screenshot_scale, ToolState};

// ── hold_keys ─────────────────────────────────────────────────────────────────

pub struct HoldKeysTool {
    pub(crate) state: Arc<ToolState>,
}

static HOLD_KEYS_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for HoldKeysTool {
    fn def(&self) -> &ToolDef {
        HOLD_KEYS_DEF.get_or_init(|| {
            let point = json!({"type":"object","required":["x","y"],"properties":{"x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0}},"additionalProperties":false});
            let pointer = json!({"type":"object","required":["action"],"properties":{
                "action":{"type":"string","enum":["click","drag"]},
                "x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0},
                "from_x":{"type":"number","minimum":0},"from_y":{"type":"number","minimum":0},
                "to_x":{"type":"number","minimum":0},"to_y":{"type":"number","minimum":0},
                "duration_ms":{"type":"integer","minimum":0,"maximum":10000},"steps":{"type":"integer","minimum":1,"maximum":200},
                "via":{"type":"array","minItems":1,"maxItems":254,"items":point}
            },"additionalProperties":false});
            ToolDef {
            name: "hold_keys".into(),
            description: "Windows only: hold 1..8 keys together for one bounded interval, optionally performing complete left clicks/drags while the keys remain down. Explicit foreground, exact pid/window_id and prior screenshot required. Keys release within this call; no cross-call state. Cancellation/target loss reports a partial prefix; never replay uncertain input.".into(),
            input_schema: json!({"type":"object","required":["pid","window_id","keys","duration_ms"],"properties":{
                "session":cua_driver_core::tool_schema::session_schema(),
                "pid":{"type":"integer","minimum":1,"description":"Process ID owning the observed target window."},
                "window_id":{"type":"integer","minimum":1,"description":"Exact native window ID from the current observation."},
                "keys":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string"},"description":"Unique (duplicates are refused) complete key names or ctrl/shift/alt/win modifiers. Modifiers press first; all action-owned keys release in reverse order."},
                "duration_ms":{"type":"integer","minimum":1,"maximum":10000,"description":"Requested interval from first injected key down through release, including pointer work; measured time is reported, not a real-time guarantee."},
                "actions":{"type":"array","minItems":1,"maxItems":32,"description":"Complete pixel left clicks/drags. All requested pointer time must fit the hold duration.","items":pointer},
                "delivery_mode":crate::input::delivery::delivery_mode_schema()
            },"additionalProperties":false}),
            read_only:false, destructive:true, idempotent:false, open_world:true,
        } })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let request = match crate::key_hold::Request::parse(&args) {
            Ok(request) => request,
            Err(code) => return path_refusal(code, "Invalid key-hold request; no input was sent."),
        };
        if crate::input::delivery::DeliveryMode::from_args(&args)
            != crate::input::delivery::DeliveryMode::Foreground
        {
            return path_refusal(
                "background_unavailable",
                "Windows key holds require explicit foreground. No input was sent.",
            );
        }
        let (pid, hwnd) = (request.pid, request.window_id);
        let ratio = match screenshot_scale(&self.state, &args, pid, Some(hwnd)) {
            Ok(ratio) => ratio,
            Err(refusal) => return refusal,
        };
        let hold = match request.into_hold(ratio) {
            Ok(hold) => hold,
            Err(code) => return path_refusal(code, "Invalid scaled hold; no input was sent."),
        };
        let started = std::time::Instant::now();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(cancelled.clone());
        // The worker keeps desktop input admission until it has released every key it holds.
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let cursor_key = resolve_cursor_key(&args);
        let result = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let report = crate::input::path::send_hold(
                hwnd,
                pid,
                &hold,
                cancelled,
                bitmap_to_screen,
                cursor_key,
            );
            (hold, report)
        })
        .await;
        match result {
            Ok((hold, Ok((report, completion)))) => complete_held_input(
                report.into_tool_result(&hold, started.elapsed().as_millis() as u64),
                completion,
            ),
            Ok((_, Err(error))) => {
                path_refusal(error.split(':').next().unwrap_or("input_failed"), &error)
            }
            Err(error) => {
                use cua_driver_core::action_record::{
                    ActionEffect, ActionExecutionRecord, ActionTransport, ActualDelivery,
                    RequestedDelivery,
                };
                let mut record = ActionExecutionRecord::new(
                    ActionEffect::Unverifiable,
                    ActionTransport::WindowsSendInput,
                    RequestedDelivery::Foreground,
                );
                record.actual_delivery = Some(ActualDelivery::Unknown);
                complete_held_input(
                    ToolResult::error(format!("Key-hold worker failed; prefix and release unconfirmed. Do not replay: {error}")).with_action_record(record),
                    crate::typing_turn::TypingTurn::Undrained,
                )
            }
        }
    }
}
