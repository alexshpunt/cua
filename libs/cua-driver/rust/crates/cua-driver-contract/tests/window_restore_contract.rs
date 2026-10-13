use cua_driver_contract::{
    tool_contract, validate_success_output, Platform, SchemaMode, ToolInput, WindowRestoreInput,
    WindowStatusInput,
};
use serde_json::{json, Value};

fn request() -> Value {
    json!({"pid":42,"window_id":123,"state_id":"state-1"})
}
fn receipt() -> Value {
    json!({
        "pid":42,"window_id":123,"state_id":"state-1",
        "restore_attempted":true,"restore_requested":true,"restored":true,
        "activation_attempted":true,"activation_succeeded":true,"visible":true,
        "foreground_at_target":true,"keyboard_focus_at_target":null,"foreground_changed":true,
        "route":"native","effect":"confirmed"
    })
}

#[test]
fn requests_bind_restoration_to_exact_window_state_without_input_coordinates() {
    serde_json::from_value::<WindowStatusInput>(json!({"pid":42,"window_id":123}))
        .unwrap()
        .validate()
        .unwrap();
    serde_json::from_value::<WindowRestoreInput>(request())
        .unwrap()
        .validate()
        .unwrap();
    for (key, value) in [
        ("pid", json!(0)),
        ("pid", json!(2147483648_u32)),
        ("window_id", json!(0)),
    ] {
        let mut value_request = request();
        value_request[key] = value.clone();
        assert_eq!(
            serde_json::from_value::<WindowRestoreInput>(value_request)
                .unwrap()
                .validate()
                .unwrap_err(),
            "restore_window requires a positive pid and exact window_id"
        );
        let mut value_request = json!({"pid":42,"window_id":123});
        value_request[key] = value;
        assert_eq!(
            serde_json::from_value::<WindowStatusInput>(value_request)
                .unwrap()
                .validate()
                .unwrap_err(),
            "get_window_status requires a positive pid and exact window_id"
        );
    }
    for state_id in ["", " \t\n"] {
        let mut invalid = request();
        invalid["state_id"] = json!(state_id);
        assert_eq!(
            serde_json::from_value::<WindowRestoreInput>(invalid)
                .unwrap()
                .validate()
                .unwrap_err(),
            "restore_window requires a nonblank state_id"
        );
    }
    for key in [
        "x",
        "capture_id",
        "element_token",
        "delivery_mode",
        "button",
    ] {
        let mut invalid = request();
        invalid[key] = json!("unrelated");
        assert_eq!(
            serde_json::from_value::<WindowRestoreInput>(invalid)
                .unwrap_err()
                .to_string(),
            format!(
                "unknown field `{key}`, expected one of `pid`, `window_id`, `state_id`, `session`"
            )
        );
    }
    let invalid = json!({"pid":42,"window_id":123,"include_screenshot":true});
    assert_eq!(
        serde_json::from_value::<WindowStatusInput>(invalid)
            .unwrap_err()
            .to_string(),
        "unknown field `include_screenshot`, expected one of `pid`, `window_id`, `session`"
    );
}

#[test]
fn status_is_readonly_windows_state_and_missing_focus_remains_unknown() {
    let status = json!({"pid":42,"window_id":123,"state_id":"state-1","minimized":true,
        "visible":true,"foreground_at_target":false,"keyboard_focus_at_target":null});
    assert_eq!(
        validate_success_output("get_window_status", status.clone()),
        Ok(true)
    );
    for (key, value, expected) in [
        (
            "pid",
            json!(0),
            "get_window_status requires a positive pid and exact window_id",
        ),
        (
            "state_id",
            json!(" "),
            "get_window_status requires a nonblank state_id",
        ),
    ] {
        let mut invalid = status.clone();
        invalid[key] = value;
        assert_eq!(
            validate_success_output("get_window_status", invalid).unwrap_err(),
            expected
        );
    }
    for name in ["get_window_status", "restore_window"] {
        let contract = tool_contract(name).unwrap();
        assert_eq!(contract.platforms, [Platform::Windows]);
        assert_eq!(contract.schema_mode, SchemaMode::CanonicalRuntime);
        assert_eq!(contract.input_schema["additionalProperties"], false);
        assert_eq!(
            contract.input_schema["properties"]["pid"]["maximum"],
            2147483647
        );
        assert_eq!(contract.annotations.read_only, name == "get_window_status");
        assert!(!contract.annotations.idempotent);
    }
}

#[test]
fn success_requires_observed_restoration_and_activation_without_claiming_fresh_pixels() {
    assert_eq!(
        validate_success_output("restore_window", receipt()),
        Ok(true)
    );
    for key in [
        "restore_attempted",
        "restore_requested",
        "restored",
        "activation_attempted",
        "activation_succeeded",
        "visible",
        "foreground_at_target",
    ] {
        let mut invalid = receipt();
        invalid[key] = json!(false);
        assert_eq!(validate_success_output("restore_window", invalid).unwrap_err(),
            "restore_window success requires requested and observed restoration, activation and visible target foreground", "{key}");
    }
    for key in ["visible", "foreground_at_target"] {
        let mut invalid = receipt();
        invalid[key] = Value::Null;
        assert_eq!(validate_success_output("restore_window", invalid).unwrap_err(),
            "restore_window success requires requested and observed restoration, activation and visible target foreground");
    }
    for (key, value, expected) in [
        (
            "pid",
            json!(2147483648_u32),
            "restore_window requires a positive pid and exact window_id",
        ),
        (
            "state_id",
            json!(""),
            "restore_window requires a nonblank state_id",
        ),
        (
            "effect",
            json!("unverifiable"),
            "unknown variant `unverifiable`, expected `confirmed`",
        ),
        (
            "route",
            json!("keyboard"),
            "unknown variant `keyboard`, expected `native`",
        ),
    ] {
        let mut invalid = receipt();
        invalid[key] = value;
        assert_eq!(
            validate_success_output("restore_window", invalid).unwrap_err(),
            expected
        );
    }
    let mut invalid = receipt();
    invalid["fresh_pixels"] = json!(true);
    assert_eq!(validate_success_output("restore_window", invalid).unwrap_err(),
        "unknown field `fresh_pixels`, expected one of `pid`, `window_id`, `state_id`, `restore_attempted`, `restore_requested`, `restored`, `activation_attempted`, `activation_succeeded`, `visible`, `foreground_at_target`, `keyboard_focus_at_target`, `foreground_changed`, `route`, `effect`");
}
