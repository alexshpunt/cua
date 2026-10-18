// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! The one place a `tools/call` result is held to the contract the driver
//! advertises.
//!
//! MCP validates every `structuredContent` a tool emits against that tool's
//! advertised `outputSchema` — payloads sent with `isError: true` included —
//! and rejects a response whose payload matches neither arm (`-32602`), or a
//! successful response that declares a schema and carries no structured
//! payload at all (`-32600`). Either way the client discards the whole
//! response, so the message the driver placed in `content` never reaches the
//! agent.
//!
//! Individual result constructors cannot enforce that: `ToolResult::error`
//! leaves `structuredContent` unset, the transport paths invent diagnostics
//! like `{"exit_code": 1}`, and the proxy synthesises an empty success when the
//! daemon answers `ok` with no result. So the rule lives here instead, on the
//! single path every direct and daemon-backed `tools/call` result passes
//! through.

use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tracing::warn;

use cua_driver_contract::{
    advertised_tool_output_schema, advertises_output_schema, conforming_error_envelope,
    is_refusal_envelope, validate_success_output,
};

/// Marker code for a result the driver replaced because the tool's own payload
/// could not be advertised under its `outputSchema`.
pub const TOOL_OUTPUT_INVALID_CODE: &str = "tool_output_invalid";

type CachedValidator = Result<Arc<jsonschema::Validator>, String>;
static OUTPUT_VALIDATORS: LazyLock<Mutex<HashMap<String, CachedValidator>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn validate_wire_output(tool: &str, structured: &Value) -> Result<(), String> {
    // Only published tools reach this cache, so arbitrary client names cannot grow it.
    let validator = OUTPUT_VALIDATORS
        .lock()
        .map_err(|_| "output schema cache unavailable".to_owned())?
        .entry(tool.to_owned())
        .or_insert_with(|| {
            let schema = advertised_tool_output_schema(tool)
                .ok_or_else(|| "advertised output schema unavailable".to_owned())?;
            jsonschema::validator_for(&schema)
                .map(Arc::new)
                .map_err(|error| format!("invalid advertised output schema: {error}"))
        })
        .clone()?;
    if validator.is_valid(structured) {
        Ok(())
    } else {
        Err("structured content does not match the advertised output schema".to_owned())
    }
}

/// Hold one `tools/call` result to the tool's advertised `outputSchema`.
///
/// - An error result keeps its diagnostic, normalized into the refusal arm so
///   a strict client accepts it and reads the `content` message.
/// - A successful result for a tool that advertises a schema must carry a
///   structured payload that the schema accepts. A missing or rejected payload
///   becomes a conforming internal-error result, because the alternative is a
///   response the client drops entirely.
/// - A successful result for a tool that advertises no schema is returned
///   unchanged: there is nothing for a client to validate it against.
pub fn conforming_tool_result(tool: &str, result: Value) -> Value {
    conforming_tool_result_inner(tool, result, advertises_output_schema(tool))
}

/// Reject contracts this proxy cannot safely normalize errors against. An older
/// daemon may omit its schema; that does not promise the local success contract.
pub fn validate_proxy_output_schema(tool: &str, schema: Option<&Value>) -> Result<(), String> {
    if let Some(schema) = schema {
        if advertised_tool_output_schema(tool).as_ref() != Some(schema) {
            return Err(format!(
                "incompatible daemon output schema for {tool}; use matching proxy and daemon versions"
            ));
        }
    }
    Ok(())
}

/// Apply the result boundary using the executing daemon's advertised contract.
/// Only matching canonical schemas enter the bounded validator cache.
pub fn conforming_proxy_tool_result(
    tool: &str,
    result: Value,
    schema: Option<&Value>,
) -> Result<Value, String> {
    validate_proxy_output_schema(tool, schema)?;
    Ok(conforming_tool_result_inner(tool, result, schema.is_some()))
}

fn conforming_tool_result_inner(tool: &str, result: Value, has_schema: bool) -> Value {
    let Value::Object(mut result) = result else {
        return internal_error_result(format!(
            "internal result mismatch for {tool}: tool result is not an object"
        ));
    };

    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        if tool == "browser_dom_action" {
            return bounded_dom_action_error(result);
        }
        let structured = result.remove("structuredContent").unwrap_or(Value::Null);
        result.insert(
            "structuredContent".to_owned(),
            conforming_error_envelope(structured),
        );
        return Value::Object(result);
    }

    if !has_schema {
        return Value::Object(result);
    }

    match result.get("structuredContent") {
        None | Some(Value::Null) => {
            warn!(
                tool,
                "successful tool result declared an output schema but carried no structured content"
            );
            invalid_output_result(tool, result, format!(
                "internal output mismatch for {tool}: tool advertises an output schema but returned no structured content"
            ))
        }
        // A refusal payload satisfies the advertised schema's other arm. The
        // typed success validator would reject it, so ask the arm it belongs
        // to rather than replacing a payload a client accepts.
        Some(structured)
            if is_refusal_envelope(structured)
                && !(tool == "browser_dom_action" && looks_like_dom_receipt(structured)) =>
        {
            Value::Object(result)
        }
        Some(structured) => match validate_wire_output(tool, structured)
            .and_then(|()| validate_success_output(tool, structured.clone()))
        {
            Ok(_) => Value::Object(result),
            Err(error) => {
                warn!(
                    tool,
                    "successful tool result carried structured content its output schema rejects"
                );
                invalid_output_result(
                    tool,
                    result,
                    format!("internal output mismatch for {tool}: {error}"),
                )
            }
        },
    }
}

fn bounded_dom_action_error(mut result: serde_json::Map<String, Value>) -> Value {
    let raw_text = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .unwrap_or("DOM action outcome unavailable. Verify state before retrying.");
    let mut bytes = 0;
    let text = raw_text
        .chars()
        .take_while(|character| {
            bytes += character.len_utf8();
            bytes <= 1024
        })
        .collect::<String>();
    let mut structured = result.remove("structuredContent").unwrap_or(Value::Null);
    let mut code = structured
        .get("code")
        .or_else(|| structured.pointer("/refusal/code"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
        .unwrap_or(TOOL_OUTPUT_INVALID_CODE)
        .to_owned();
    let receipt_fields = [
        "format",
        "action_result",
        "observed",
        "input_route",
        "trusted",
        "atomic",
        "native_pixel_mapping",
        "limitations",
    ];
    let prefix = if receipt_fields
        .iter()
        .any(|field| structured.get(field).is_some())
    {
        match cua_driver_contract::DomActionResult::from_value(structured) {
            Ok(receipt) => Some(receipt.progress),
            Err(_) => {
                code = TOOL_OUTPUT_INVALID_CODE.to_owned();
                None
            }
        }
    } else if let Some(value) = structured
        .as_object_mut()
        .and_then(|object| object.remove("progress"))
    {
        match cua_driver_contract::DomActionProgress::from_value(value) {
            Ok(prefix) => Some(prefix),
            Err(_) => {
                code = TOOL_OUTPUT_INVALID_CODE.to_owned();
                None
            }
        }
    } else {
        None
    };
    let mut diagnostic = json!({"code":code,"execution_state":"unknown"});
    if let Some(prefix) = prefix {
        diagnostic["progress"] = serde_json::to_value(prefix).expect("closed prefix serializes");
    }
    json!({"isError":true,"content":[{"type":"text","text":text}],"structuredContent":diagnostic})
}

fn looks_like_dom_receipt(value: &Value) -> bool {
    [
        "format",
        "progress",
        "action_result",
        "observed",
        "input_route",
        "trusted",
        "atomic",
        "native_pixel_mapping",
        "limitations",
    ]
    .iter()
    .any(|field| value.get(field).is_some())
}

fn invalid_output_result(
    tool: &str,
    original: serde_json::Map<String, Value>,
    message: String,
) -> Value {
    // Rejected DOM receipts may be oversized or carry raw native identities. Never echo
    // them or their prose as accepted evidence through an otherwise open error envelope.
    if tool == "browser_dom_action" {
        return internal_error_result(message);
    }
    let mut result = internal_error_result(message);
    if let Some(Value::Array(content)) = original.get("content") {
        result["content"]
            .as_array_mut()
            .unwrap()
            .extend(content.clone());
    }
    if let Some(structured) = original.get("structuredContent") {
        // Preserve evidence as an explicitly invalid diagnostic, never as success.
        result["structuredContent"]["invalid_output"] = structured.clone();
    }
    result
}

/// Build the `isError: true` result for a call that produced no usable tool
/// payload, already normalized into the refusal arm.
pub fn internal_error_result(message: impl Into<String>) -> Value {
    let message = format!(
        "{}; the tool may have executed. Verify state before retrying.",
        message.into()
    );
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
        "structuredContent": {"code": TOOL_OUTPUT_INVALID_CODE, "execution_state": "unknown"},
    })
}

/// Build a tool-level error result carrying a caller-supplied diagnostic.
///
/// The diagnostic is passed through untouched here; [`conforming_tool_result`]
/// is what normalizes it, so every error path gets the same treatment whether
/// or not it went through this constructor.
pub fn tool_error_result(message: impl Into<String>, structured: Value) -> Value {
    let message = message.into();
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
        "structuredContent": structured,
    })
}

#[cfg(test)]
mod tests {
    //! The boundary is asserted the way a strict MCP client sees it: every
    //! payload leaving it is validated against the schema the tool actually
    //! advertises.

    use super::*;
    use cua_driver_contract::{advertised_tool_output_schema, TOOL_INVOCATION_FAILED_CODE};

    /// An action tool, so the advertised schema is the shared `ActionResult`
    /// one plus the refusal arm.
    const ACTION_TOOL: &str = "click";
    /// Advertises no `outputSchema` at all.
    const UNSCHEMED_TOOL: &str = "no_such_tool";

    #[track_caller]
    fn assert_conforms(tool: &str, result: &Value) {
        let schema = advertised_tool_output_schema(tool).expect("tool advertises a schema");
        let compiled = jsonschema::validator_for(&schema).expect("schema compiles");
        let structured = &result["structuredContent"];

        assert!(
            compiled.is_valid(structured),
            "advertised schema rejected {structured}"
        );
    }

    fn window_control_sample() -> Value {
        json!({
            "bounds": {"x": 139, "y": 120, "width": 840, "height": 1000},
            "bounds_source": "dwm_visible_frame",
            "coordinate_space": "windows_desktop_pixels",
            "minimized": false, "maximized": false, "visible": true,
            "foreground_at_target": true, "foreground_window": 91,
            "keyboard_focus_at_target": null,
            "virtual_desktop": {
                "desktop_id": "40497ffa-fd1a-4f18-8ae0-bcdf44d1dc26",
                "desktop_id_error": null, "on_current_desktop": true,
                "on_current_desktop_error": null, "sampled_at_unix_ms": 1
            }
        })
    }

    fn management_receipt(operation: &str) -> Value {
        let before = window_control_sample();
        let mut observed = before.clone();
        if operation == "minimize" {
            observed["minimized"] = json!(true);
        } else if operation == "maximize" {
            observed["maximized"] = json!(true);
        }
        json!({
            "pid": 42, "window_id": 91, "state_id": "management-wire-test",
            "operation": operation, "requested_bounds": null,
            "mutation_attempted": true, "mutation_accepted": true,
            "postcondition_confirmed": true, "before": before, "observed": observed,
            "route": "native", "effect": "confirmed"
        })
    }

    fn window_success(structured: Value) -> Value {
        json!({"content": [], "isError": false, "structuredContent": structured})
    }

    #[test]
    fn window_management_wire_accepts_unknown_keyboard_focus() {
        let success = window_success(json!({
            "pid": 42, "window_id": 91, "state_id": "management-wire-test",
            "state": window_control_sample()
        }));
        let result = conforming_tool_result("get_window_control_state", success.clone());
        assert_conforms("get_window_control_state", &result);
        assert_eq!(result, success);
    }

    #[test]
    fn window_management_wire_accepts_non_frame_receipts() {
        for operation in ["focus", "minimize", "maximize"] {
            for focus in [Value::Null, json!(true)] {
                let mut structured = management_receipt(operation);
                structured["before"]["keyboard_focus_at_target"] = focus.clone();
                structured["observed"]["keyboard_focus_at_target"] = focus;
                assert!(validate_success_output("manage_window", structured.clone()).is_ok());
                let success = window_success(structured);
                let result = conforming_tool_result("manage_window", success.clone());
                assert_conforms("manage_window", &result);
                assert_eq!(result, success, "valid {operation} receipt rejected");
            }
        }
    }

    #[test]
    fn window_management_wire_requires_nullable_keys_and_valid_postconditions() {
        let mut cases = Vec::new();
        for key in ["requested_bounds", "before", "observed"] {
            let mut receipt = management_receipt("focus");
            receipt.as_object_mut().unwrap().remove(key);
            cases.push(receipt);
        }
        let mut missing_focus = management_receipt("focus");
        missing_focus["observed"]
            .as_object_mut()
            .unwrap()
            .remove("keyboard_focus_at_target");
        cases.push(missing_focus);
        let mut missing_before = management_receipt("focus");
        missing_before["before"] = Value::Null;
        cases.push(missing_before);
        let mut wrong_postcondition = management_receipt("minimize");
        wrong_postcondition["observed"]["minimized"] = json!(false);
        cases.push(wrong_postcondition);
        for receipt in cases {
            let result = conforming_tool_result("manage_window", window_success(receipt));
            assert_conforms("manage_window", &result);
            assert_eq!(result["isError"], true);
            assert_eq!(
                result["structuredContent"]["code"],
                TOOL_OUTPUT_INVALID_CODE
            );
        }
    }
    /// The shape this boundary exists for: the bare diagnostic the transport
    /// paths invent satisfies neither arm, which is what produced the `-32602`
    /// reports.
    #[test]
    fn a_bare_exit_code_diagnostic_does_not_validate() {
        let schema = advertised_tool_output_schema(ACTION_TOOL).expect("schema");
        let compiled = jsonschema::validator_for(&schema).expect("schema compiles");

        assert!(!compiled.is_valid(&json!({"exit_code": 1})));
    }

    #[test]
    fn an_error_diagnostic_gains_a_refusal_marker() {
        let result = conforming_tool_result(
            ACTION_TOOL,
            tool_error_result("daemon transport closed", json!({"exit_code": 1})),
        );

        assert_conforms(ACTION_TOOL, &result);
        // The diagnostic survives; only the marker is added.
        assert_eq!(result["structuredContent"]["exit_code"], 1);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_INVOCATION_FAILED_CODE
        );
        assert_eq!(result["content"][0]["text"], "daemon transport closed");
    }

    /// `ToolResult::error` leaves `structuredContent` unset, and every tool
    /// that refuses through it lands here.
    #[test]
    fn an_error_without_any_payload_gains_one() {
        let result = conforming_tool_result(
            ACTION_TOOL,
            json!({"content": [{"type": "text", "text": "Unknown tool: nope"}], "isError": true}),
        );

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_INVOCATION_FAILED_CODE
        );
    }

    /// The guard adds a marker; it does not relabel a payload that already
    /// names its own refusal.
    #[test]
    fn an_existing_refusal_code_is_preserved() {
        let result = conforming_tool_result(
            ACTION_TOOL,
            tool_error_result("denied", json!({"code": "permission_denied"})),
        );

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(result["structuredContent"]["code"], "permission_denied");
    }

    /// The proxy's empty-success fallback: a client that was promised a schema
    /// rejects this response outright rather than reading it as an error.
    #[test]
    fn a_success_missing_its_structured_payload_becomes_an_error() {
        let result = conforming_tool_result(ACTION_TOOL, json!({"content": [], "isError": false}));

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_OUTPUT_INVALID_CODE
        );
    }

    #[test]
    fn a_success_whose_payload_the_schema_rejects_becomes_an_error() {
        let result = conforming_tool_result(
            ACTION_TOOL,
            json!({
                "content": [],
                "isError": false,
                "structuredContent": {"clicked": true},
            }),
        );

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_OUTPUT_INVALID_CODE
        );
    }

    #[test]
    fn a_conforming_success_is_returned_untouched() {
        let structured = json!({
            "effect": "confirmed",
            "route": "accessibility",
            "delivery": {"mode": "background"},
            "evidence": [{"kind": "value_readback"}],
        });
        let success = json!({
            "content": [{"type": "text", "text": "clicked"}],
            "isError": false,
            "structuredContent": structured,
        });

        let result = conforming_tool_result(ACTION_TOOL, success.clone());

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(result, success);
    }

    #[test]
    fn a_success_missing_required_nullable_fields_becomes_a_conforming_error() {
        let structured = json!({
            "session": "schema-test",
            "capture_scope": "window",
            "effective_scope": "window",
            "desktop_capture_authorized": false,
            "desktop_unlocked": false,
        });
        let tool = "get_session_state";
        assert!(validate_success_output(tool, structured.clone()).is_ok());
        let result = conforming_tool_result(
            tool,
            json!({
                "content": [{"type": "text", "text": "action completed"}],
                "isError": false,
                "structuredContent": structured,
            }),
        );

        assert_conforms(tool, &result);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_OUTPUT_INVALID_CODE
        );
        assert_eq!(result["structuredContent"]["execution_state"], "unknown");
        assert_eq!(
            result["structuredContent"]["invalid_output"]["session"],
            "schema-test"
        );
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Verify state before retrying"));
        assert_eq!(result["content"][1]["text"], "action completed");
    }

    fn agent_cursor_state(position: Value) -> Value {
        json!({
            "session": "cursor-state-test",
            "enabled": true,
            "position": position,
            "theme": {
                "id": "cua-default",
                "version": "1",
                "profile": "default",
                "reduced_motion": "auto",
                "fallback": null,
            },
            "visual_state": {
                "requested_action": "idle",
                "resolved_action": "idle",
                "modifiers": [],
                "phase": "idle",
                "frame": 0,
                "preempted_count": 0,
            },
            "motion": {
                "start_handle": 0.3,
                "end_handle": 0.3,
                "arc_size": 0.25,
                "arc_flow": 0.0,
                "spring": 0.72,
                "glide_duration_ms": 750.0,
                "dwell_after_click_ms": 400.0,
                "idle_hide_ms": 3000.0,
                "turn_radius": 12.0,
                "style": "signature_arc",
                "timing": "native",
                "effects": {
                    "trail": false,
                    "glow": true,
                    "magnet": false,
                    "ripple": true,
                    "squish": true,
                },
            },
        })
    }

    /// A session cursor that has never moved has no position yet. Every
    /// platform reports that as `"position": null`, and the advertised schema
    /// must accept it rather than turning the read into `tool_output_invalid`.
    #[test]
    fn agent_cursor_state_accepts_a_cursor_that_has_not_moved() {
        const TOOL: &str = "get_agent_cursor_state";
        for position in [Value::Null, json!({"x": 10.0, "y": 20.0})] {
            let success = json!({
                "content": [{"type": "text", "text": "Agent cursor state."}],
                "isError": false,
                "structuredContent": agent_cursor_state(position),
            });

            let result = conforming_tool_result(TOOL, success.clone());

            assert_conforms(TOOL, &result);
            assert_eq!(result, success);
        }
    }

    /// `position` stays required: an unmoved cursor reports `null`, it does
    /// not drop the key.
    #[test]
    fn agent_cursor_state_without_a_position_key_is_rejected() {
        const TOOL: &str = "get_agent_cursor_state";
        let mut structured = agent_cursor_state(Value::Null);
        structured.as_object_mut().unwrap().remove("position");

        let result = conforming_tool_result(
            TOOL,
            json!({"content": [], "isError": false, "structuredContent": structured}),
        );

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["code"],
            TOOL_OUTPUT_INVALID_CODE
        );
    }

    /// A tool with no advertised schema has no contract to violate, so its
    /// successful payload must not be second-guessed here.
    #[test]
    fn a_success_from_a_tool_without_a_schema_is_untouched() {
        let success = json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false,
            "structuredContent": {"anything": "goes"},
        });

        assert_eq!(
            conforming_tool_result(UNSCHEMED_TOOL, success.clone()),
            success
        );
    }

    #[test]
    fn a_result_that_is_not_an_object_becomes_a_conforming_error() {
        let result = conforming_tool_result(ACTION_TOOL, json!("not a result"));

        assert_conforms(ACTION_TOOL, &result);
        assert_eq!(result["isError"], true);
    }
}
