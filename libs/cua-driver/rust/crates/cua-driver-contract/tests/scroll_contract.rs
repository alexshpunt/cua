use cua_driver_contract::{
    tool_contract, validate_success_output, InputDeliveryMode, Platform, PointerScrollInput,
    ToolInput,
};
use serde_json::{json, Value};

fn request() -> Value {
    json!({
        "pid":42,"window_id":123,"capture_id":"capture-1","x":0.5,"y":20.0,
        "delta_x":2,"delta_y":-3,"unit":"wheel_detents"
    })
}

fn parse(value: Value) -> PointerScrollInput {
    serde_json::from_value(value).unwrap()
}

fn receipt() -> Value {
    json!({
        "attempted":true,"delivered":true,"pointer_attempted":true,"pointer_delivered":true,
        "delta_x":2,"delta_y":-3,"unit":"wheel_detents",
        "attempted_axes":["y","x"],"delivered_axes":["y","x"],
        "route":"native","effect":"unverifiable","delivery_mode":"foreground",
        "foreground_changed":true,"pointer_changed":true,
        "foreground_at_target":true,"pointer_at_target":true
    })
}

#[test]
fn wheel_requests_require_an_exact_capture_point_and_bounded_nonzero_deltas() {
    let input = parse(request());
    input.validate().unwrap();
    assert_eq!(input.delivery_mode, InputDeliveryMode::Background);
    let mut one_axis = request();
    one_axis.as_object_mut().unwrap().remove("delta_x");
    let input = parse(one_axis);
    assert_eq!(input.delta_x, 0);
    input.validate().unwrap();
    for (key, value, expected) in [
        (
            "pid",
            json!(0),
            "scroll_at requires a positive pid and exact window_id",
        ),
        (
            "pid",
            json!(2147483648_u32),
            "scroll_at requires a positive pid and exact window_id",
        ),
        (
            "window_id",
            json!(0),
            "scroll_at requires a positive pid and exact window_id",
        ),
        (
            "capture_id",
            json!(" "),
            "scroll_at requires a nonblank capture_id",
        ),
        (
            "x",
            json!(-1),
            "scroll_at coordinates must be finite and nonnegative",
        ),
        (
            "y",
            json!(-1),
            "scroll_at coordinates must be finite and nonnegative",
        ),
        (
            "delta_x",
            json!(51),
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
        (
            "delta_y",
            json!(-51),
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
    ] {
        let mut invalid = request();
        invalid[key] = value;
        assert_eq!(parse(invalid).validate().unwrap_err(), expected, "{key}");
    }
    let mut zero = request();
    zero["delta_x"] = json!(0);
    zero["delta_y"] = json!(0);
    assert_eq!(
        parse(zero).validate().unwrap_err(),
        "scroll_at requires a nonzero delta"
    );
    for coordinate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut invalid = parse(request());
        invalid.x = coordinate;
        assert_eq!(
            invalid.validate().unwrap_err(),
            "scroll_at coordinates must be finite and nonnegative"
        );
    }
}

#[test]
fn the_wire_contract_cannot_widen_to_desktop_tokens_pixels_or_fractional_detents() {
    const FIELDS: &str = "`pid`, `window_id`, `capture_id`, `x`, `y`, `delta_x`, `delta_y`, `unit`, `delivery_mode`, `session`";
    for (key, value, expected) in [
        (
            "scope",
            json!("desktop"),
            format!("unknown field `scope`, expected one of {FIELDS}"),
        ),
        (
            "element_token",
            json!("s00000001:2"),
            format!("unknown field `element_token`, expected one of {FIELDS}"),
        ),
        (
            "by",
            json!("page"),
            format!("unknown field `by`, expected one of {FIELDS}"),
        ),
        (
            "amount",
            json!(3),
            format!("unknown field `amount`, expected one of {FIELDS}"),
        ),
        (
            "modifiers",
            json!(["ctrl"]),
            format!("unknown field `modifiers`, expected one of {FIELDS}"),
        ),
        (
            "dwell_ms",
            json!(100),
            format!("unknown field `dwell_ms`, expected one of {FIELDS}"),
        ),
        (
            "unit",
            json!("pixels"),
            "unknown variant `pixels`, expected `wheel_detents`".into(),
        ),
        (
            "delta_x",
            json!(1.5),
            "invalid type: floating point `1.5`, expected i32".into(),
        ),
        (
            "delivery_mode",
            json!("auto"),
            "unknown variant `auto`, expected `background` or `foreground`".into(),
        ),
    ] {
        let mut invalid = request();
        invalid[key] = value;
        assert_eq!(
            serde_json::from_value::<PointerScrollInput>(invalid)
                .unwrap_err()
                .to_string(),
            expected,
            "{key}"
        );
    }
    for field in ["pid", "window_id", "capture_id", "x", "y", "unit"] {
        let mut invalid = request();
        invalid.as_object_mut().unwrap().remove(field);
        assert_eq!(
            serde_json::from_value::<PointerScrollInput>(invalid)
                .unwrap_err()
                .to_string(),
            format!("missing field `{field}`")
        );
    }
    let contract = tool_contract("scroll_at").unwrap();
    assert_eq!(contract.platforms, vec![Platform::Windows]);
    assert_eq!(contract.input_schema["additionalProperties"], false);
    assert!(contract.input_schema.get("oneOf").is_none());
    assert_eq!(
        contract.input_schema["properties"]["delta_x"]["minimum"],
        -50
    );
    assert_eq!(
        contract.input_schema["properties"]["delta_y"]["maximum"],
        50
    );
}

#[test]
fn success_requires_the_exact_completed_axis_prefix_and_cannot_claim_content_displacement() {
    assert_eq!(validate_success_output("scroll_at", receipt()), Ok(true));
    for flag in [
        "attempted",
        "delivered",
        "pointer_attempted",
        "pointer_delivered",
    ] {
        let mut invalid = receipt();
        invalid[flag] = json!(false);
        assert_eq!(
            validate_success_output("scroll_at", invalid).unwrap_err(),
            "scroll_at success requires attempted and delivered pointer and wheel input",
            "{flag}"
        );
    }
    for (field, axes) in [
        ("attempted_axes", json!(["y"])),
        ("delivered_axes", json!(["y"])),
        ("delivered_axes", json!(["x", "y"])),
        ("attempted_axes", json!(["y", "x", "x"])),
    ] {
        let mut invalid = receipt();
        invalid[field] = axes;
        assert_eq!(
            validate_success_output("scroll_at", invalid).unwrap_err(),
            "scroll_at success requires the complete requested axis sequence",
            "{field}"
        );
    }
    for (field, value, expected) in [
        (
            "route",
            json!("overlay"),
            "unknown variant `overlay`, expected `native`",
        ),
        (
            "effect",
            json!("confirmed"),
            "unknown variant `confirmed`, expected `unverifiable`",
        ),
        (
            "delivery_mode",
            json!("background"),
            "unknown variant `background`, expected `foreground`",
        ),
        (
            "unit",
            json!("pixels"),
            "unknown variant `pixels`, expected `wheel_detents`",
        ),
        (
            "delta_x",
            json!(51),
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
    ] {
        let mut invalid = receipt();
        invalid[field] = value;
        assert_eq!(
            validate_success_output("scroll_at", invalid).unwrap_err(),
            expected,
            "{field}"
        );
    }
    let mut zero = receipt();
    zero["delta_x"] = json!(0);
    zero["delta_y"] = json!(0);
    assert_eq!(
        validate_success_output("scroll_at", zero).unwrap_err(),
        "scroll_at requires a nonzero delta"
    );
    for (zero_delta, axis) in [("delta_x", "y"), ("delta_y", "x")] {
        let mut one_axis = receipt();
        one_axis[zero_delta] = json!(0);
        one_axis["attempted_axes"] = json!([axis]);
        one_axis["delivered_axes"] = json!([axis]);
        one_axis["foreground_changed"] = Value::Null;
        one_axis["pointer_changed"] = Value::Null;
        one_axis["foreground_at_target"] = Value::Null;
        one_axis["pointer_at_target"] = Value::Null;
        assert_eq!(validate_success_output("scroll_at", one_axis), Ok(true));
    }
    let mut unrelated = receipt();
    unrelated["content_pixels"] = json!(80);
    assert_eq!(
        validate_success_output("scroll_at", unrelated).unwrap_err(),
        "unknown field `content_pixels`, expected one of `attempted`, `delivered`, `pointer_attempted`, `pointer_delivered`, `delta_x`, `delta_y`, `unit`, `attempted_axes`, `delivered_axes`, `route`, `effect`, `delivery_mode`, `foreground_changed`, `pointer_changed`, `foreground_at_target`, `pointer_at_target`"
    );
}
