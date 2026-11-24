// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

use async_trait::async_trait;
use cua_driver_contract::HotkeyInput;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::parse_typed_projection,
};
use serde_json::{json, Value};
use std::sync::Arc;

use super::impl_::{
    focus_by_pixel, focus_cached_element_for_foreground, position_keyboard_cursor, ToolState,
};

// ── hotkey ────────────────────────────────────────────────────────────────────

fn is_modifier(k: &str) -> bool {
    matches!(
        k.to_lowercase().as_str(),
        "ctrl" | "control" | "shift" | "alt" | "win" | "windows" | "cmd" | "command"
    )
}

pub struct HotkeyTool {
    pub(crate) state: Arc<ToolState>,
}
static HOTKEY_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for HotkeyTool {
    fn def(&self) -> &ToolDef {
        HOTKEY_DEF.get_or_init(|| ToolDef {
            name: "hotkey".into(),
            // Description ported from Swift `HotkeyTool.swift` with the
            // Windows-specific dispatch (posted keys with thread key state in the
            // background, SendInput in the foreground).
            description: "Press a combination of keys simultaneously — e.g. `[\"ctrl\", \"c\"]` \
                for Copy, `[\"ctrl\", \"shift\", \"t\"]` for reopen-closed-tab. Dispatch:\n\n\
                - **delivery_mode:\"background\"** (default) posts the key to the target's \
                keyboard window (its focused child, a XAML island's input site, a Chromium \
                frame) and holds the modifiers in the target thread's key state, so \
                `GetKeyState` and `TranslateAccelerator` see the chord. No focus steal; the \
                global keyboard state and the foreground stay. A key that opens a Win32 menu \
                loop is closed again and reported as background_unavailable.\n\n\
                - **delivery_mode:\"foreground\"** routes through `SendInput` against the \
                system input queue, with a brief `SetForegroundWindow` swap so the events land \
                on the target. Foreground return requires supported receiver-read evidence for \
                every inserted event and no older unconfirmed input; otherwise the target stays \
                in front. Windows can deny activation under foreground-lock or integrity rules.\n\n\
                The target does NOT need to be frontmost in the background.\n\n\
                **`window_id`** (optional): explicit HWND when the pid owns more than one \
                window; otherwise the pid's first visible window is used.\n\n\
                Recognized modifiers: ctrl/control, shift, alt, win/windows. Non-modifier \
                keys use the same vocabulary as `press_key` (return, tab, escape, \
                up/down/left/right, space, delete, home, end, pageup, pagedown, f1-f12, \
                letters, digits). Order: modifiers first, one non-modifier last.".into(),
            input_schema: json!({
                "type":"object","required":["keys"],"properties":{
                    "session": cua_driver_core::tool_schema::session_schema(),
                    "scope":{"type":"string","enum":["window","desktop"],"description":"Use desktop with no pid/window_id to send the hotkey to the current foreground application."},
                    "pid":{"type":"integer","description":"Target process ID."},
                    "keys":{"type":"array","items":{"type":"string"},"minItems":2,
                        "description":"Modifier(s) and one non-modifier key, e.g. [\"ctrl\", \"c\"]."},
                    "element_token": cua_driver_core::tool_schema::element_token_schema(),
                    "x":{"type":"number","description":"Window-local screenshot-pixel X — the element px action form: pixel-click there to focus, then send the combo (so e.g. Ctrl+V pastes into that field). Pass with y. Use for Chromium/Electron surfaces the background combo can't reach."},
                    "y":{"type":"number","description":"Window-local screenshot-pixel Y (see x)."},
                    "window_id":{"type":"integer","description":"Explicit HWND when the pid owns multiple windows."},
                    "delivery_mode": crate::input::delivery::delivery_mode_schema()
                },"additionalProperties":false
            }),
            read_only: false, destructive: true, idempotent: false, open_world: true,
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        use crate::input::delivery::{DeliveryMode, EventKind};
        use cua_driver_core::tool_args::ArgsExt;
        if args.get("scope").and_then(Value::as_str) == Some("desktop")
            && args.get("pid").is_none()
            && args.get("window_id").is_none()
        {
            let input = match parse_typed_projection::<HotkeyInput>("hotkey", &args) {
                Ok(input) => input,
                Err(result) => return result,
            };
            if input.keys.len() < 2 {
                return ToolResult::error("hotkey.keys must contain at least two keys.")
                    .with_structured(json!({ "code": "invalid_arguments" }));
            }
            let mods: Vec<String> = input
                .keys
                .iter()
                .filter(|key| is_modifier(key))
                .cloned()
                .collect();
            let Some(key) = input
                .keys
                .iter()
                .rev()
                .find(|key| !is_modifier(key))
                .cloned()
            else {
                return ToolResult::error("keys must include at least one non-modifier key.");
            };
            let full_keys = input.keys;
            let hwnd = match crate::input::mouse::foreground_window() {
                Ok(hwnd) => hwnd,
                Err(error) => return ToolResult::error(error.to_string()),
            };
            position_keyboard_cursor(&self.state, &args, hwnd, None).await;
            let key_display = full_keys.join("+");
            return match tokio::task::spawn_blocking(move || {
                let modifiers: Vec<&str> = mods.iter().map(String::as_str).collect();
                crate::input::send_key_synthesized(hwnd, &key, &modifiers)
            })
            .await
            {
                Ok(Ok(turn)) => crate::typing_turn::completion_result(
                    ToolResult::text(format!(
                        "Sent {key_display} to the foreground application (scope:desktop)."
                    ))
                    .with_structured(json!({
                        "scope": "desktop", "path": "SendInput", "keys": full_keys,
                        "effect": "unverifiable"
                    })),
                    turn,
                ),
                Ok(Err(error)) => crate::typing_turn::foreground_input_error(error),
                Err(error) => ToolResult::error(format!("desktop hotkey task failed: {error}")),
            };
        }
        // Parse keys array (Swift's only path).  Legacy key+modifiers shape
        // still accepted as a Rust-side convenience.
        let (key, mods, full_keys) = if let Some(arr) = args.get("keys") {
            let raw = match arr.as_array() {
                Some(r) => r,
                None => return ToolResult::error("Missing required array field keys."),
            };
            let keys: Vec<String> = raw
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            if keys.len() != raw.len() || keys.is_empty() {
                return ToolResult::error("keys must be a non-empty array of strings.");
            }
            let modifiers: Vec<String> = keys.iter().filter(|k| is_modifier(k)).cloned().collect();
            let non_mods: Vec<String> = keys.iter().filter(|k| !is_modifier(k)).cloned().collect();
            if non_mods.is_empty() {
                return ToolResult::error("keys must include at least one non-modifier key.");
            }
            (non_mods.last().unwrap().clone(), modifiers, keys)
        } else if let Some(k) = args.get("key").and_then(|v| v.as_str()) {
            let m: Vec<String> = args
                .get("modifiers")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut full = m.clone();
            full.push(k.to_owned());
            (k.to_owned(), m, full)
        } else {
            return ToolResult::error("Missing required array field keys.");
        };
        let raw_pid = match args.require_i64("pid") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let pid = raw_pid as u32;
        let resolved = match self.state.snapshots.resolve(pid as i32, &args) {
            Ok(resolved) => resolved,
            Err(error) => return error,
        };
        let (elem_idx, hwnd_opt, admitted) = resolved.into_parts(args.opt_u64("window_id"));
        let delivery = DeliveryMode::from_args(&args);

        // Resolve HWND: use window_id if given, else pick first window for pid.
        let hwnd = match hwnd_opt {
            Some(h) => h,
            None => {
                let pid2 = pid;
                let windows = tokio::task::spawn_blocking({
                    let admitted = admitted.clone();
                    move || {
                        let _admission = &admitted;
                        crate::win32::list_windows(Some(pid2))
                    }
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

        // Match Swift's text format exactly: `"✅ Pressed K1+K2+... on pid X."`
        // where K1+K2+... is `keys.joined(separator: "+")` of the FULL keys
        // array as the caller supplied it (modifiers + non-modifier in order).
        let key_display = full_keys.join("+");

        // px form: pixel-click to focus, then the combo acts on the focused field
        // (e.g. Ctrl+V into a Chromium input). A background focus click is posted
        // like any background click; the chord follows on the posted route.
        let px_focus = {
            let px = args.get("x").and_then(|v| v.as_f64());
            let py = args.get("y").and_then(|v| v.as_f64());
            if let (Some(cx), Some(cy)) = (px, py) {
                if elem_idx.is_some() {
                    return ToolResult::error(
                        "Pass either element_token (ax) or x,y (px) to hotkey, not both.",
                    );
                }
                let from_zoom = args
                    .get("from_zoom")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let Err(e) = focus_by_pixel(
                    &self.state,
                    pid,
                    hwnd_opt,
                    cx,
                    cy,
                    delivery.is_foreground(),
                    args.opt_str("session"),
                    args.opt_str("_session_id"),
                    from_zoom,
                    false,
                )
                .await
                {
                    return e;
                }
                true
            } else {
                false
            }
        };

        // The px form's focus click already moved the cursor. Otherwise place
        // a named session's cursor so a keyboard-first session stays visible.
        if !px_focus {
            let element_center = admitted
                .as_ref()
                .map(|element| (element.center.0 as f64, element.center.1 as f64));
            position_keyboard_cursor(&self.state, &args, hwnd, element_center).await;
        }

        // Routing decisions:
        //   1. Background: PostMessage WM_KEYDOWN/UP to the keyboard target (the
        //      focused child, a XAML island's input site, Chromium's top-level
        //      window), with the modifiers held in the target thread's key state
        //      so GetKeyState and TranslateAccelerator see the chord (LPT-1846).
        //      No foreground swap, no focus theft, global key state untouched.
        //   2. Foreground: SendInput synthesized hotkey on the system input
        //      queue, with a brief foreground swap.
        let has_modifiers = !mods.is_empty();
        // delivery_mode:"background" — refuse if PostMessage of the key combo
        // would be silently dropped on this target (see the class table in
        // `input::delivery::would_be_silently_dropped`).
        let event_kind = if has_modifiers {
            EventKind::KeyCombo
        } else {
            EventKind::Keystroke
        };
        if delivery == DeliveryMode::Background
            && crate::input::delivery::would_be_silently_dropped(hwnd, event_kind)
        {
            // macOS-aligned: a `background` hotkey never fronts. This combo would
            // be silently dropped and only a foreground/focus grab lands it —
            // which background must not do. Surface background_unavailable; the
            // agent escalates to delivery_mode:"foreground".
            return crate::input::delivery::background_unavailable_error(hwnd, event_kind);
        }
        // delivery_mode:"foreground" — explicit SendInput swap (the path that
        // unblocks TranslateAccelerator-style apps). A Background call that
        // reaches here means the key combo is NOT silently dropped on this
        // target (the drop-check above returned early otherwise), so it stays
        // on PostMessage and the no-foreground contract holds.
        let use_send_input = delivery == DeliveryMode::Foreground;
        let focus_target = elem_idx.map(|idx| {
            let point = admitted.as_ref().map(|element| element.center);
            (idx, point)
        });
        let result = tokio::task::spawn_blocking({
            let admitted = admitted.clone();
            move || {
                let _admission = &admitted;
                let m: Vec<&str> = mods.iter().map(String::as_str).collect();
                if use_send_input {
                    let prior = if px_focus {
                        crate::typing_turn::TypingTurn::Unobservable
                    } else {
                        crate::typing_turn::TypingTurn::NotSent
                    };
                    crate::input::send_key_synthesized_after_focus(hwnd, &key, &m, prior, || {
                        if let Some((idx, point)) = focus_target {
                            focus_cached_element_for_foreground(&admitted, hwnd, idx, point)
                        } else {
                            Ok(crate::typing_turn::TypingTurn::NotSent)
                        }
                    })
                } else {
                    crate::input::post_key(hwnd, &key, &m)
                        .map(|()| crate::typing_turn::TypingTurn::NotSent)
                }
            }
        })
        .await;
        let path = if use_send_input {
            "SendInput"
        } else {
            "PostMessage"
        };
        match result {
            Ok(Ok(turn)) => {
                let result = ToolResult::text(format!(
                    "✅ Pressed {key_display} on pid {raw_pid} via {path} (Win32 target)."
                ));
                if use_send_input {
                    crate::typing_turn::completion_result(result, turn)
                } else {
                    result
                }
            }
            Ok(Err(e)) if use_send_input => crate::typing_turn::foreground_input_error(e),
            Ok(Err(e)) => crate::input::delivery::posted_input_error(hwnd, event_kind, e),
            Err(e) => ToolResult::error(format!("Task error: {e}")),
        }
    }
}
