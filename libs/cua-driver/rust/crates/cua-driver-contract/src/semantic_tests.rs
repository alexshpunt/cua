use crate::{tool_contract, Platform};
use serde_json::json;

#[test]
fn semantic_value_and_identity_are_validated_before_native_dispatch() {
    use crate::{SemanticActionInput, ToolInput};
    let request =
        json!({"pid":42,"window_id":123,"element_token":"s00000001:2","operation":"invoke"});
    let parse = |value| serde_json::from_value::<SemanticActionInput>(value);
    assert!(parse(request.clone()).unwrap().validate().is_ok());
    for (key, value) in [
        ("pid", json!(0)),
        ("window_id", json!(0)),
        ("element_token", json!("")),
        ("value", json!("unexpected")),
    ] {
        let mut invalid = request.clone();
        invalid[key] = value;
        assert!(parse(invalid).unwrap().validate().is_err(), "{key}");
    }
    for (key, value) in [("x", json!(0)), ("delivery_mode", json!("background"))] {
        let mut invalid = request.clone();
        invalid[key] = value;
        assert!(parse(invalid).is_err(), "{key}");
    }
    let mut write = request;
    write["operation"] = json!("set_value");
    assert!(parse(write.clone()).unwrap().validate().is_err());
    write["value"] = json!("");
    assert!(parse(write).unwrap().validate().is_ok());
}

#[test]
fn semantic_requests_cannot_request_coordinates_or_foreground_fallback() {
    let contract = tool_contract("semantic_action").expect("strict semantic contract");
    assert_eq!(contract.platforms, vec![Platform::Windows]);
    assert_eq!(contract.input_schema["additionalProperties"], false);
    for key in ["x", "delivery_mode", "scope"] {
        assert!(
            contract.input_schema["properties"].get(key).is_none(),
            "{key}"
        );
    }
}

#[test]
fn semantic_receipt_does_not_claim_a_verified_app_effect() {
    let contract = tool_contract("semantic_action").expect("strict semantic contract");
    let receipt = json!({"operation":"invoke","pattern":"invoke","route":"accessibility","effect":"unverifiable","delivery":{"mode":"background"}});
    assert!((contract.output_validator)(receipt.clone()).is_ok());
    for (key, value) in [
        ("route", json!("synthetic_events")),
        ("effect", json!("confirmed")),
        ("delivery", json!({"mode":"foreground"})),
    ] {
        let mut invalid = receipt.clone();
        invalid[key] = value;
        assert!((contract.output_validator)(invalid).is_err(), "{key}");
    }
}
