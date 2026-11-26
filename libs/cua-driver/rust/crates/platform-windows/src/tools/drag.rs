// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

use async_trait::async_trait;
use cua_driver_contract::{ClickButton, DragInput};
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::parse_typed_projection,
};
use serde_json::{json, Value};
use std::sync::Arc;

use super::impl_::{
    bitmap_to_screen, capture_admission_refusal, overlay_glide_to, pin_overlay_above,
    resolve_cursor_key, screenshot_scale, winui3_background_gesture, ToolState,
};

async fn track_overlay_drag(
    key: String,
    from: (f64, f64),
    to: (f64, f64),
    duration_ms: u64,
    steps: usize,
) {
    if key.is_empty() || !crate::overlay::is_enabled(&key) {
        return;
    }
    let _pressed = cursor_overlay::PressedVisualGuard::new(|command| {
        crate::overlay::send_command(key.clone(), command);
    });
    let steps = steps.max(1);
    let step_delay = std::time::Duration::from_millis(duration_ms / steps as u64);
    for index in 0..=steps {
        let t = index as f64 / steps as f64;
        let x = from.0 + (to.0 - from.0) * t;
        let y = from.1 + (to.1 - from.1) * t;
        crate::overlay::send_command(key.clone(), cursor_overlay::track_pointer_command(x, y));
        if index < steps && !step_delay.is_zero() {
            tokio::time::sleep(step_delay).await;
        }
    }
    // The guard releases artwork on native completion, cancellation or normal end.
}

// ── drag ─────────────────────────────────────────────────────────────────────

pub struct DragTool {
    pub(crate) state: Arc<ToolState>,
}

static DRAG_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

/// A structured refusal of held input (paths, key holds, timelines): nothing was sent.
pub(super) fn path_refusal(code: &str, message: &str) -> ToolResult {
    complete_held_input(
        ToolResult::error(message).with_structured(json!({"code":code,"effect":"refused"})),
        crate::typing_turn::TypingTurn::NotSent,
    )
}

/// Carry stream read uncertainty without replacing the insertion/action receipt.
pub(super) fn complete_held_input(
    result: ToolResult,
    completion: crate::typing_turn::TypingTurn,
) -> ToolResult {
    crate::typing_turn::completion_result(result, completion)
}

impl DragTool {
    async fn invoke_path(&self, args: &Value, cursor_key: String) -> ToolResult {
        let request = match crate::drag_path::Request::parse(args) {
            Ok(request) => request,
            Err(code) => {
                return path_refusal(code, "Invalid held-path request; no input was sent.")
            }
        };
        if crate::input::delivery::DeliveryMode::from_args(args)
            != crate::input::delivery::DeliveryMode::Foreground
        {
            return path_refusal(
                "background_unavailable",
                "Windows held paths need delivery_mode:foreground. No path input was sent.",
            );
        }
        let pid = request.pid;
        let hwnd = request.window_id;
        let ratio = match screenshot_scale(&self.state, args, pid, Some(hwnd)) {
            Ok(ratio) => ratio,
            Err(refusal) => return refusal,
        };
        let path = match request.into_path(ratio) {
            Ok(path) => path,
            Err(code) => return path_refusal(code, "Invalid scaled path; no input was sent."),
        };
        let total_points = path.points.len();
        let duration_ms = path.duration_ms;
        let started = std::time::Instant::now();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(cancelled.clone());
        // A cancelled caller must not admit another input while this worker
        // still owns its button. The worker releases before dropping the lease.
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let result = cua_driver_core::desktop_activity::spawn_blocking(move || {
            let _admission = admission;
            crate::input::path::send_path(hwnd, pid, &path, cancelled, bitmap_to_screen, cursor_key)
        })
        .await;
        match result {
            Ok(Ok((report, completion))) => complete_held_input(
                report.into_tool_result(
                    total_points,
                    duration_ms,
                    started.elapsed().as_millis() as u64,
                ),
                completion,
            ),
            Ok(Err(error)) => {
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
                    ToolResult::error(format!("Held-path worker failed; prefix and release unconfirmed. Do not replay: {error}"))
                        .with_action_record(record),
                    crate::typing_turn::TypingTurn::Undrained,
                )
            }
        }
    }
}

#[async_trait]
impl Tool for DragTool {
    fn def(&self) -> &ToolDef {
        DRAG_DEF.get_or_init(|| ToolDef {
            name: "drag".into(),
            description: "Press-drag-release gesture from (from_x, from_y) to (to_x, to_y) in window-local screenshot pixels. \
                          duration_ms (default 500) is the wall-clock budget; steps (default 20) interpolates intermediate \
                          WM_MOUSEMOVE events along the straight path. Optional via (1–254 intermediate {x,y} points) holds one \
                          left button across a Windows foreground path, requiring explicit pid/window_id and a prior screenshot; \
                          incompatible with steps, modifier, or from_zoom. Straight background drags do not steal focus. \
                          Optional capture_id binds a straight drag to one prior capture of the exact pid/window_id; both endpoints and live dimensions must match before input. It cannot be combined with via, desktop scope or zoom. \
                          Foreground delivery activates the target and refuses before pressing when another window covers the start point. \
                          After a zoom call, straight drags may pass from_zoom=true. \
                          Drags that start on a window caption / title bar or resize border (i.e. moving or resizing the \
                          window itself) cannot be delivered in the background — the OS move/resize loop needs real pointer \
                          input — so they return background_unavailable; re-issue those with delivery_mode:\"foreground\". \
                          `modifier` holds keys from press to release in delivery_mode:\"foreground\" and scope:\"desktop\" via \
                          SendInput. Background drags with modifiers return background_unavailable because posted messages and \
                          pen injection cannot carry live modifier-key state.".into(),
            input_schema: json!({"type":"object","required":["from_x","from_y","to_x","to_y"],"properties":{
                "session": cua_driver_core::tool_schema::session_schema(),
                "scope":{"type":"string","enum":["window","desktop"],"description":"Use desktop with no pid/window_id for screen-absolute coordinates."},
                "pid":{"type":"integer","description":"Target process ID."},
                "window_id":{"type":"integer","description":"Target window handle (HWND). Optional — driver picks frontmost window of pid when omitted."},
                "capture_id":{"type":"string","minLength":1,"description":"Windows only: bind both straight-drag endpoints to this window capture. Requires exact pid/window_id; no via, desktop or zoom."},
                "from_x":{"type":"number","description":"Drag-start X in window-local screenshot pixels."},
                "from_y":{"type":"number","description":"Drag-start Y in window-local screenshot pixels."},
                "to_x":{"type":"number","description":"Drag-end X in window-local screenshot pixels."},
                "to_y":{"type":"number","description":"Drag-end Y in window-local screenshot pixels."},
                "via":{"type":"array","minItems":1,"maxItems":254,"description":"Windows only: intermediate screenshot-space points for one held left-button foreground path. Requires explicit pid/window_id; no steps, modifier or zoom coordinates.","items":{"type":"object","required":["x","y"],"properties":{"x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0}},"additionalProperties":false}},
                "duration_ms":{"type":"integer","minimum":0,"maximum":10000,"description":"Wall-clock duration of drag path. Default: 500."},
                "steps":{"type":"integer","minimum":1,"maximum":200,"description":"Number of intermediate WM_MOUSEMOVE events. Default: 20."},
                "modifier": cua_driver_core::tool_schema::modifier_schema(),
                "button": cua_driver_core::tool_schema::button_schema(),
                "from_zoom":{"type":"boolean","description":"When true, coordinates are in the last zoom image for this pid."},
                "delivery_mode": crate::input::delivery::delivery_mode_schema()
            },"additionalProperties":false}),
            read_only: false, destructive: true, idempotent: false, open_world: true,
        })
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        use crate::input::delivery::{DeliveryMode, EventKind};
        use cua_driver_core::tool_args::ArgsExt;
        let cursor_key = resolve_cursor_key(&args);
        let capture_bound = args.get("capture_id").is_some();
        if capture_bound
            && (args
                .get("capture_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
                || args.get("via").is_some()
                || args
                    .get("scope")
                    .and_then(Value::as_str)
                    .is_some_and(|scope| scope != "window")
                || args.get("from_zoom").and_then(Value::as_bool) == Some(true)
                || args
                    .get("pid")
                    .and_then(Value::as_u64)
                    .is_none_or(|pid| pid == 0 || pid > u64::from(u32::MAX))
                || args
                    .get("window_id")
                    .and_then(Value::as_u64)
                    .is_none_or(|hwnd| hwnd == 0))
        {
            return capture_admission_refusal(anyhow::anyhow!(
                "capture_id requires an exact pid/window_id and straight window screenshot coordinates; via, desktop and zoom are not capture-bound"
            ));
        }
        if args.get("via").is_some() {
            return self.invoke_path(&args, cursor_key).await;
        }
        if args.get("scope").and_then(Value::as_str) == Some("desktop")
            && args.get("pid").is_none()
            && args.get("window_id").is_none()
        {
            let input = match parse_typed_projection::<DragInput>("drag", &args) {
                Ok(input) => input,
                Err(error) => return error,
            };
            let from_x = input.from_x as i32;
            let from_y = input.from_y as i32;
            let to_x = input.to_x as i32;
            let to_y = input.to_y as i32;
            let duration_ms = input.duration_ms.unwrap_or(500);
            let steps = input.steps.unwrap_or(20) as usize;
            let button = input.button.unwrap_or(ClickButton::Left).as_str();
            let modifiers = input.modifier.unwrap_or_default();
            let hwnd = match crate::input::mouse::foreground_window() {
                Ok(hwnd) => hwnd,
                Err(error) => return ToolResult::error(error.to_string()),
            };
            let native_drag = cua_driver_core::desktop_activity::spawn_blocking(move || {
                let modifier_refs: Vec<&str> = modifiers.iter().map(String::as_str).collect();
                crate::input::mouse::send_drag_synthesized(
                    hwnd,
                    from_x,
                    from_y,
                    to_x,
                    to_y,
                    duration_ms,
                    steps,
                    button,
                    &modifier_refs,
                )
            });
            let visual_drag = track_overlay_drag(
                cursor_key.clone(),
                (f64::from(from_x), f64::from(from_y)),
                (f64::from(to_x), f64::from(to_y)),
                duration_ms,
                steps,
            );
            let native_drag =
                crate::activity::alongside_presentation(native_drag, visual_drag).await;
            return match native_drag {
                Ok(Ok(_)) => complete_held_input(
                    ToolResult::text(format!(
                        "Dragged from ({from_x}, {from_y}) to ({to_x}, {to_y}) (scope:desktop)."
                    ))
                    .with_structured(json!({
                        "scope": "desktop", "path": "SendInput",
                        "from": {"x": from_x, "y": from_y}, "to": {"x": to_x, "y": to_y},
                        "effect": "unverifiable"
                    })),
                    crate::typing_turn::TypingTurn::Unobservable,
                ),
                Ok(Err(error)) => complete_held_input(
                    crate::input::occlusion::input_error(error),
                    crate::typing_turn::TypingTurn::Undrained,
                ),
                Err(error) => complete_held_input(
                    ToolResult::error(format!("desktop drag task failed: {error}")),
                    crate::typing_turn::TypingTurn::Undrained,
                ),
            };
        }
        // Swift error wording 1:1.
        let raw_pid = match args.get("pid").and_then(|v| v.as_i64()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required integer field pid."),
        };
        let pid = raw_pid as u32;
        let delivery = DeliveryMode::from_args(&args);
        // Held keys: real keys in the foreground, the target thread's key state for a
        // posted background drag.
        let modifiers: Vec<String> = args.str_array("modifier");

        // Accepts numeric JSON as either float or integer — coerce both to f64.
        let coerce = |key: &str| -> Option<f64> {
            args.opt_f64(key)
                .or_else(|| args.opt_i64(key).map(|i| i as f64))
        };
        let from_x_opt = coerce("from_x");
        let from_y_opt = coerce("from_y");
        let to_x_opt = coerce("to_x");
        let to_y_opt = coerce("to_y");
        let (mut from_x, mut from_y, mut to_x, mut to_y) =
            match (from_x_opt, from_y_opt, to_x_opt, to_y_opt) {
                (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
                _ => {
                    return ToolResult::error(
                        "from_x, from_y, to_x, and to_y are all required (window-local pixels).",
                    )
                }
            };

        let hwnd_opt = args.opt_u64("window_id");
        let duration_ms = args.u64_or("duration_ms", 500);
        let steps = args.u64_or("steps", 20) as usize;
        let button = args.str_or("button", "left");
        let from_zoom = args.bool_or("from_zoom", false);

        if capture_bound {
            let Some(bridge) = &self.state.capture_bridge else {
                return capture_admission_refusal(anyhow::anyhow!(
                    "capture service is unavailable"
                ));
            };
            match bridge.admit_drag(
                &args,
                crate::capture_admission::WindowsCaptureTarget::Window {
                    pid,
                    window_id: hwnd_opt.unwrap(),
                },
                (from_x, from_y),
                (to_x, to_y),
            ) {
                Ok((from, to)) => {
                    (from_x, from_y) = from;
                    (to_x, to_y) = to;
                }
                Err(error) => return capture_admission_refusal(error),
            }
        } else if from_zoom {
            match self.state.zoom_context(&args, pid, hwnd_opt) {
                Ok(ctx) => {
                    let (wx, wy) = ctx.zoom_to_window(from_x, from_y);
                    let (wx2, wy2) = ctx.zoom_to_window(to_x, to_y);
                    from_x = wx;
                    from_y = wy;
                    to_x = wx2;
                    to_y = wy2;
                }
                Err(refusal) => return refusal,
            }
        } else {
            let ratio = match screenshot_scale(&self.state, &args, pid, hwnd_opt) {
                Ok(ratio) => ratio,
                Err(refusal) => return refusal,
            };
            from_x *= ratio;
            from_y *= ratio;
            to_x *= ratio;
            to_y *= ratio;
        }

        let hwnd = match hwnd_opt {
            Some(h) => h,
            None => {
                let windows = cua_driver_core::desktop_activity::spawn_blocking(move || {
                    crate::win32::list_windows(Some(pid))
                })
                .await
                .unwrap_or_default();
                match windows.first() {
                    Some(w) => w.hwnd,
                    None => {
                        return ToolResult::error(format!(
                            "No windows found for pid {pid}. Provide window_id."
                        ))
                    }
                }
            }
        };

        // Compute screen-coord endpoints. Same correction as the click
        // tools — bitmap pixels are anchored to the DWM-frame top-left,
        // not the client area top-left (see `bitmap_to_screen` doc).
        let (from, to) = if capture_bound {
            let from = match crate::capture_admission::round_action_point(from_x, from_y) {
                Ok(point) => point,
                Err(error) => return capture_admission_refusal(error),
            };
            let to = match crate::capture_admission::round_action_point(to_x, to_y) {
                Ok(point) => point,
                Err(error) => return capture_admission_refusal(error),
            };
            (from, to)
        } else {
            ((from_x as i32, from_y as i32), (to_x as i32, to_y as i32))
        };
        let (sx_from, sy_from) = bitmap_to_screen(hwnd, from.0, from.1);
        let (sx_to, sy_to) = bitmap_to_screen(hwnd, to.0, to.1);

        // delivery_mode:"background" on WinUI3: a pointer drag can't both land and
        // hold the contract — the content island only consumes real
        // system-queue pointer input (which the pen/touch injector delivers but
        // at the cost of click-activating the frame: 8/8 foreground steals) and
        // ignores posted WM_* drag messages; UIA has no drag analogue. Return
        // the structured error so the caller retries with delivery_mode:foreground
        // (a WinUI3 Slider can also be set with the set_value tool, which uses
        // UIA RangeValuePattern and holds the contract).
        if delivery.is_background() {
            if let Some(r) = winui3_background_gesture(&None, pid, hwnd, None, 1, &button).await {
                return r;
            }
        }

        // Chromium can accept a posted press but lose later drag phases after
        // pointer capture. Refuse before the press rather than leaving a partial
        // gesture behind; foreground delivery remains available.
        if delivery.is_background() && crate::input::is_chromium_target_window(hwnd) {
            return crate::input::delivery::background_unavailable_error(
                hwnd,
                EventKind::MouseMove,
            );
        }
        // Synthetic background input cannot guarantee cursor/focus posture.
        // Refuse before input rather than restoring an unread stream by timer.
        if delivery.is_background()
            && crate::input::delivery::would_be_silently_dropped(hwnd, EventKind::MouseMove)
        {
            return crate::input::delivery::background_pointer_unavailable_error(
                hwnd,
                EventKind::MouseMove,
            );
        }
        // delivery_mode:"foreground" — SendInput-based drag. Required for WPF
        // Slider thumbs (and any framework that polls GetKeyState during
        // its drag handler — PostMessage doesn't update per-thread input
        // state, so those targets see a button-up world during the drag
        // and never start tracking). Like the foreground click, it activates
        // the target and refuses rather than drag in a window that covers it.
        if delivery == DeliveryMode::Foreground {
            let btn_fg = button.clone();
            pin_overlay_above(&cursor_key, hwnd);
            let send_result = cua_driver_core::desktop_activity::spawn_blocking(move || {
                let modifier_refs: Vec<&str> = modifiers.iter().map(String::as_str).collect();
                crate::input::mouse::send_drag_synthesized_active(
                    hwnd,
                    sx_from,
                    sy_from,
                    sx_to,
                    sy_to,
                    duration_ms,
                    steps,
                    &btn_fg,
                    &modifier_refs,
                )
            });
            let visual_drag = track_overlay_drag(
                cursor_key.clone(),
                (f64::from(sx_from), f64::from(sy_from)),
                (f64::from(sx_to), f64::from(sy_to)),
                duration_ms,
                steps,
            );
            let send_result =
                crate::activity::alongside_presentation(send_result, visual_drag).await;
            let button_suffix = if button == "left" {
                String::new()
            } else {
                format!(" ({} button)", button)
            };
            return match send_result {
                // Drained when the target was seen reading the press and the release.
                Ok(Ok(read)) => complete_held_input(
                    ToolResult::text(format!(
                        "✅ Sent drag{button_suffix} via SendInput on pid {raw_pid} \
                         from screen ({sx_from},{sy_from}) → ({sx_to},{sy_to}) \
                         in {duration_ms}ms / {steps} steps (delivery_mode:foreground)."
                    )),
                    read,
                ),
                Ok(Err(e)) => complete_held_input(
                    crate::input::occlusion::input_error(e),
                    crate::typing_turn::TypingTurn::Undrained,
                ),
                Err(e) => complete_held_input(
                    ToolResult::error(format!("Task error: {e}")),
                    crate::typing_turn::TypingTurn::Undrained,
                ),
            };
        }

        // Background posted-message drags cannot move or resize a top-level
        // window: a caption/border drag is handled by the OS's interactive
        // move/resize modal loop, which only real pointer input can drive —
        // the posted WM_MOUSEMOVE stream is discarded and the "success" is a
        // verified no-op. Hit-test the start point and refuse with the
        // structured background_unavailable error so callers escalate to
        // delivery_mode:"foreground" per the documented ladder instead of
        // trusting a drag that did nothing.
        let nc_hit = cua_driver_core::desktop_activity::spawn_blocking(move || {
            crate::input::mouse::non_client_move_resize_hit(hwnd, sx_from, sy_from)
        })
        .await
        .ok()
        .flatten();
        if let Some(region) = nc_hit {
            return crate::input::delivery::background_unavailable_error_with_cause(
                hwnd,
                EventKind::MouseMove,
                format!(
                    "the drag starts on the window's {region} (WM_NCHITTEST), and moving \
                     or resizing a top-level window requires the OS interactive \
                     move/resize loop, which posted mouse messages cannot drive. \
                     Re-issue the same drag with delivery_mode:\"foreground\"."
                ),
            );
        }

        // Pin the agent-cursor overlay above the drag target so the synthetic
        // cursor stays sandwiched at z+1 of the dragged window for the full
        // path. Drag stays within a single HWND, so one pin at the start is
        // sufficient — the 80 ms z-order tick keeps it asserted thereafter.
        pin_overlay_above(&cursor_key, hwnd);

        // Move the agent cursor to the drag-start, show the press state, and
        // track the same interpolated step count and duration while the
        // posted-message drag runs.
        overlay_glide_to(&cursor_key, sx_from as f64, sy_from as f64).await;
        crate::overlay::send_command(
            cursor_key.clone(),
            cursor_overlay::OverlayCommand::ClickPulse {
                x: sx_from as f64,
                y: sy_from as f64,
            },
        );

        let button_c = button.clone();
        let result = cua_driver_core::desktop_activity::spawn_blocking(move || {
            // Screen-coord, deepest-child variant: routes the gesture to the
            // child control under the start point (e.g. a WinForms Panel),
            // not the top-level frame that would ignore it.
            let modifier_refs: Vec<&str> = modifiers.iter().map(String::as_str).collect();
            crate::input::mouse::post_drag_screen(
                hwnd,
                sx_from,
                sy_from,
                sx_to,
                sy_to,
                duration_ms,
                steps,
                &button_c,
                &modifier_refs,
            )
        });
        let visual_drag = track_overlay_drag(
            cursor_key.clone(),
            (f64::from(sx_from), f64::from(sy_from)),
            (f64::from(sx_to), f64::from(sy_to)),
            duration_ms,
            steps,
        );
        let result = crate::activity::alongside_presentation(result, visual_drag).await;

        match result {
            Ok(Ok(())) => {
                // Match Swift's text format verbatim.
                let button_suffix = if button == "left" {
                    String::new()
                } else {
                    format!(" ({} button)", button)
                };
                ToolResult::text(format!(
                    "✅ Posted drag{button_suffix} to pid {raw_pid} \
                     from window-pixel ({}, {}) → ({}, {}), \
                     screen ({sx_from}, {sy_from}) → ({sx_to}, {sy_to}) \
                     in {duration_ms}ms / {steps} steps.",
                    from_x as i32, from_y as i32, to_x as i32, to_y as i32,
                ))
            }
            Ok(Err(e)) => crate::input::delivery::posted_input_error(hwnd, EventKind::MouseMove, e),
            Err(e) => ToolResult::error(format!("Task error: {e}")),
        }
    }
}
