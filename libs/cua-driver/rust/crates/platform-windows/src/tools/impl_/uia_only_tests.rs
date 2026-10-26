use super::{uia_only_refusal, ClickTool, ScrollTool};
use cua_driver_core::tool::Tool;
use serde_json::json;

#[test]
fn uia_only_needs_an_element_and_background_delivery() {
    assert!(uia_only_refusal(&json!({"pid": 1, "window_id": 2, "x": 3, "y": 4}), false).is_none());
    assert!(uia_only_refusal(&json!({"element_token": "s00000001:2"}), true).is_none());
    assert!(uia_only_refusal(&json!({"element_token": "s00000001:2", "delivery_mode": "background"}), true).is_none());
    for args in [
        json!({"pid": 1, "window_id": 2, "x": 3, "y": 4}),
        json!({"element_token": null}),
        json!({"element_token": "s00000001:2", "delivery_mode": "foreground"}),
    ] {
        let refusal = uia_only_refusal(&args, true).expect("uia_only must refuse before any dispatch");
        assert!(refusal.is_error.unwrap_or(false), "{args}");
    }
}

#[test]
fn click_and_scroll_schemas_accept_uia_only() {
    let click = ClickTool { state: super::ToolState::new(None) };
    let scroll = ScrollTool { state: super::ToolState::new(None) };
    for schema in [&click.def().input_schema, &scroll.def().input_schema] {
        assert_eq!(schema["properties"]["uia_only"]["type"], "boolean");
        assert_eq!(schema["additionalProperties"], false);
    }
}
