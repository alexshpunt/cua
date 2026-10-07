use super::{CursorRegistry, DragTool, DriverConfig, Snapshots, ToolState};
use cua_driver_core::tool::Tool;
use serde_json::json;
use std::sync::{Arc, RwLock};

fn tool() -> DragTool {
    DragTool {
        state: Arc::new(ToolState {
            snapshots: Arc::new(Snapshots::new()),
            cursor_registry: Arc::new(CursorRegistry::new()),
            config: Arc::new(RwLock::new(DriverConfig::default())),
            capture_bridge: None,
        }),
    }
}

#[test]
fn straight_drag_schema_accepts_a_non_empty_capture_id() {
    let tool = tool();
    let capture_id = &tool.def().input_schema["properties"]["capture_id"];
    assert_eq!(capture_id["type"], "string");
    assert_eq!(capture_id["minLength"], 1);
}

#[tokio::test]
async fn capture_bound_drag_refuses_unsupported_routes_before_native_work() {
    let tool = tool();
    for extra in [
        json!({"scope":"desktop"}),
        json!({"via":[{"x":2,"y":2}]}),
        json!({"from_zoom":true}),
        json!({"window_id":null}),
        json!({"pid":null}),
        json!({"pid":-1}),
        json!({"capture_id":null}),
    ] {
        let mut args = json!({"pid":42,"window_id":7,"capture_id":"unused", "from_x":1,"from_y":1,"to_x":3,"to_y":3});
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        let result = tool.invoke(args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "capture_action_refused"
        );
        assert!(result.structured_content.as_ref().unwrap()["detail"]
            .as_str()
            .unwrap()
            .contains("capture_id requires"));
    }
    let result = tool.invoke(json!({"pid":42,"window_id":7,"capture_id":"unused", "from_x":1,"from_y":1,"to_x":3,"to_y":3})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["detail"],
        "capture service is unavailable"
    );
}
