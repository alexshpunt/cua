use super::{GetWindowStateTool, ToolState};
use cua_driver_core::tool::Tool;

// T3 Code feature-detects the preview mode from this schema property.
#[test]
fn display_only_is_advertised() {
    let tool = GetWindowStateTool {
        state: ToolState::new(None),
    };
    assert_eq!(
        tool.def().input_schema["properties"]["display_only"]["type"],
        "boolean"
    );
}

#[tokio::test]
async fn display_only_refuses_accessibility_snapshot_requests() {
    let tool = GetWindowStateTool {
        state: ToolState::new(None),
    };
    for tree in [None, Some(true)] {
        let mut args = serde_json::json!({"pid": 42, "window_id": 7, "display_only": true});
        if let Some(tree) = tree {
            args["include_accessibility_tree"] = serde_json::json!(tree);
        }
        let result = tool.invoke(args).await;
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::to_value(cua_driver_core::protocol::ToolResult::error(
                "display_only requires include_accessibility_tree:false"
            ))
            .unwrap()
        );
    }
}

#[test]
fn a_crop_is_advertised_and_parsed_only_for_display_only_reads() {
    let tool = GetWindowStateTool {
        state: ToolState::new(None),
    };
    assert_eq!(
        tool.def().input_schema["properties"]["crop"]["type"],
        "object"
    );
    let crop = serde_json::json!({"crop": {"x": 10, "y": 20, "width": 300, "height": 200}});
    assert_eq!(
        super::window_crop(&crop, true).unwrap(),
        Some(cua_driver_core::image_utils::PixelRegion {
            x: 10,
            y: 20,
            width: 300,
            height: 200
        })
    );
    assert_eq!(
        super::window_crop(&serde_json::json!({}), true).unwrap(),
        None
    );
    assert!(super::window_crop(&crop, false).is_err());
    let empty = serde_json::json!({"crop": {"x": 0, "y": 0, "width": 0, "height": 5}});
    assert!(super::window_crop(&empty, true).is_err());
    let partial = serde_json::json!({"crop": {"x": 0, "y": 0}});
    assert!(super::window_crop(&partial, true).is_err());
}
