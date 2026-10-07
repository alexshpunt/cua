use super::*;
use serde_json::json;

#[test]
fn zero_guid_becomes_unknown_without_erasing_current_desktop() {
    let sample = VirtualDesktopMembership::from_queries(
        Ok("00000000-0000-0000-0000-000000000000".into()),
        Ok(false),
        42,
    );
    sample.validate().unwrap();
    assert_eq!(sample.desktop_id, None);
    assert_eq!(
        sample.desktop_id_error.unwrap().code,
        crate::DesktopQueryErrorCode::InvalidDesktopId
    );
    assert_eq!(sample.on_current_desktop, Some(false));
    assert_eq!(sample.on_current_desktop_error, None);
}
fn membership() -> serde_json::Value {
    json!({
        "desktop_id":"00112233-4455-6677-8899-aabbccddeeff",
        "on_current_desktop":false,
        "desktop_id_error":null,
        "on_current_desktop_error":null,
        "sampled_at_unix_ms":42
    })
}

#[test]
fn window_results_preserve_independently_sampled_shell_membership() {
    let observation = json!({"pid":7,"window_id":9,"virtual_desktop":membership()});
    let output: WindowStateOutput = serde_json::from_value(observation.clone()).unwrap();
    output.validate().unwrap();
    assert_eq!(
        serde_json::to_value(output).unwrap()["virtual_desktop"],
        observation["virtual_desktop"]
    );
    let window = json!({"pid":7,"window_id":9,"app_name":"Fixture","title":"Fixture",
        "bounds":{"x":0,"y":0,"width":100,"height":100},"is_on_screen":false,"z_index":null,
        "virtual_desktop":membership()});
    let output: ListWindowsOutput =
        serde_json::from_value(json!({"windows":[window.clone()]})).unwrap();
    output.validate().unwrap();
    assert_eq!(
        serde_json::to_value(output).unwrap()["windows"][0]["virtual_desktop"],
        window["virtual_desktop"]
    );
    assert!(WindowStateOutput::output_schema()["properties"]
        .get("virtual_desktop")
        .is_some());
}

#[test]
fn invalid_shell_membership_is_not_a_success_shape() {
    for patch in [
        json!({"desktop_id":"00000000-0000-0000-0000-000000000000"}),
        json!({"desktop_id":"current"}),
        json!({"desktop_id":null}),
        json!({"on_current_desktop":null}),
        json!({"desktop_id_error":{"code":"desktop_id_query_failed","hresult":-1}}),
    ] {
        let mut value = membership();
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let output: WindowStateOutput =
            serde_json::from_value(json!({"pid":7,"window_id":9,"virtual_desktop":value})).unwrap();
        assert!(
            output.validate().is_err(),
            "invalid metadata must not validate: {patch}"
        );
    }
}

#[test]
fn query_failure_does_not_erase_the_other_successful_field() {
    for value in [
        json!({"desktop_id":null,"on_current_desktop":false,
            "desktop_id_error":{"code":"desktop_id_query_failed","hresult":-2147024891},
            "on_current_desktop_error":null,"sampled_at_unix_ms":42}),
        json!({"desktop_id":"00112233-4455-6677-8899-aabbccddeeff","on_current_desktop":null,
            "desktop_id_error":null,"on_current_desktop_error":{"code":"current_desktop_query_failed","hresult":-1},
            "sampled_at_unix_ms":42}),
    ] {
        let output: WindowStateOutput =
            serde_json::from_value(json!({"pid":7,"window_id":9,"virtual_desktop":value.clone()}))
                .unwrap();
        output.validate().unwrap();
        assert_eq!(
            serde_json::to_value(output).unwrap()["virtual_desktop"],
            value
        );
    }
}
