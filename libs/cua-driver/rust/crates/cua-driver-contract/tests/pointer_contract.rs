use cua_driver_contract::{
    tool_contract, validate_success_output, InputDeliveryMode, Platform, PointerMoveInput,
    ToolInput,
};
use serde_json::json;

fn capture_request() -> serde_json::Value {
    json!({"pid":42,"window_id":123,"capture_id":"capture-1","x":0.5,"y":20.0})
}

#[test]
fn native_pointer_requests_require_one_exact_admitted_target() {
    let parse = |value| serde_json::from_value::<PointerMoveInput>(value).unwrap();
    let input = parse(capture_request());
    input.validate().unwrap();
    assert_eq!(input.delivery_mode, InputDeliveryMode::Background);
    assert_eq!(input.dwell_ms, 0);
    parse(json!({"pid":42,"window_id":123,"element_token":"s00000001:2","delivery_mode":"foreground","dwell_ms":10000}))
        .validate().unwrap();
    for (key, value, expected) in [
        (
            "pid",
            json!(0),
            "move_pointer requires a positive pid and exact window_id",
        ),
        (
            "pid",
            json!(2147483648_u32),
            "move_pointer requires a positive pid and exact window_id",
        ),
        (
            "window_id",
            json!(0),
            "move_pointer requires a positive pid and exact window_id",
        ),
        (
            "capture_id",
            json!(""),
            "move_pointer requires exactly one capture point or element token",
        ),
        (
            "x",
            json!(-1),
            "move_pointer coordinates must be finite and nonnegative",
        ),
        (
            "dwell_ms",
            json!(10001),
            "move_pointer dwell_ms must be at most 10000",
        ),
        (
            "element_token",
            json!("s00000001:2"),
            "move_pointer requires exactly one capture point or element token",
        ),
    ] {
        let mut invalid = capture_request();
        invalid[key] = value;
        assert_eq!(parse(invalid).validate().unwrap_err(), expected, "{key}");
    }
    for invalid in [
        json!({"pid":42,"window_id":123}),
        json!({"pid":42,"window_id":123,"x":1,"y":2}),
        json!({"pid":42,"window_id":123,"capture_id":"c","x":1}),
        json!({"pid":42,"window_id":123,"element_token":" "}),
    ] {
        assert_eq!(
            parse(invalid).validate().unwrap_err(),
            "move_pointer requires exactly one capture point or element token"
        );
    }
    let mut invalid = input;
    invalid.x = Some(f64::INFINITY);
    assert_eq!(
        invalid.validate().unwrap_err(),
        "move_pointer coordinates must be finite and nonnegative"
    );
}

#[test]
fn requests_reject_unbounded_dwell_and_unrelated_input_fields() {
    const FIELDS: &str = "`pid`, `window_id`, `capture_id`, `x`, `y`, `element_token`, `dwell_ms`, `delivery_mode`, `session`";
    for (key, value, expected) in [
        (
            "dwell_ms",
            json!(-1),
            "invalid value: integer `-1`, expected u64".to_owned(),
        ),
        (
            "dwell_ms",
            json!(1.5),
            "invalid type: floating point `1.5`, expected u64".to_owned(),
        ),
        (
            "button",
            json!("left"),
            format!("unknown field `button`, expected one of {FIELDS}"),
        ),
        (
            "scope",
            json!("desktop"),
            format!("unknown field `scope`, expected one of {FIELDS}"),
        ),
        (
            "from_zoom",
            json!(true),
            format!("unknown field `from_zoom`, expected one of {FIELDS}"),
        ),
        (
            "delivery_mode",
            json!("auto"),
            "unknown variant `auto`, expected `background` or `foreground`".to_owned(),
        ),
    ] {
        let mut invalid = capture_request();
        invalid[key] = value;
        assert_eq!(
            serde_json::from_value::<PointerMoveInput>(invalid)
                .unwrap_err()
                .to_string(),
            expected,
            "{key}"
        );
    }
    let contract = tool_contract("move_pointer").unwrap();
    assert_eq!(contract.platforms, vec![Platform::Windows]);
    assert_eq!(contract.input_schema["additionalProperties"], false);
    assert_eq!(
        contract.input_schema["properties"]["dwell_ms"]["maximum"],
        10000
    );
    assert_eq!(contract.input_schema["oneOf"].as_array().unwrap().len(), 2);
}

#[test]
fn native_receipts_cannot_claim_a_tooltip_or_hide_incomplete_dwell() {
    let receipt = json!({
        "attempted":true,"delivered":true,"route":"native","effect":"unverifiable",
        "delivery_mode":"foreground","dwell_ms":25,"dwell_elapsed_ms":27,
        "foreground_changed":true,"pointer_changed":true,
        "foreground_at_target":true,"pointer_at_target":true
    });
    assert_eq!(
        validate_success_output("move_pointer", receipt.clone()),
        Ok(true)
    );
    for (key, value, expected) in [
        ("attempted", json!(false), "move_pointer success requires attempted and delivered native motion"),
        ("delivered", json!(false), "move_pointer success requires attempted and delivered native motion"),
        ("effect", json!("confirmed"), "unknown variant `confirmed`, expected `unverifiable`"),
        ("route", json!("overlay"), "unknown variant `overlay`, expected `native`"),
        ("delivery_mode", json!("background"), "unknown variant `background`, expected `foreground`"),
        ("dwell_elapsed_ms", json!(24), "move_pointer success requires the requested dwell to complete"),
        ("dwell_ms", json!(10001), "move_pointer dwell_ms must be at most 10000"),
        ("button_events", json!(1), "unknown field `button_events`, expected one of `attempted`, `delivered`, `route`, `effect`, `delivery_mode`, `dwell_ms`, `dwell_elapsed_ms`, `foreground_changed`, `pointer_changed`, `foreground_at_target`, `pointer_at_target`"),
    ] {
        let mut invalid = receipt.clone();
        invalid[key] = value;
        assert_eq!(validate_success_output("move_pointer", invalid).unwrap_err(), expected, "{key}");
    }
}
