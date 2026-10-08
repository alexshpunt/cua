//! Public strict operation refusal without reading user state or invoking host UIA.
use super::super::semantic::SemanticActionTool;
use super::*;

fn tool() -> SemanticActionTool {
    SemanticActionTool {
        state: Arc::new(ToolState {
            snapshots: Arc::new(Snapshots::new()),
            cursor_registry: Arc::new(CursorRegistry::new()),
            config: Arc::new(RwLock::new(DriverConfig::default())),
            capture_bridge: None,
        }),
    }
}

#[tokio::test]
async fn strict_semantic_admission_refuses_stale_tokens_and_physical_arguments() {
    let tool = tool();
    let args = json!({"pid":42,"window_id":7,"element_token":"s00000001:0","operation":"invoke"});
    let result = tool.invoke(args.clone()).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["refusal"]["code"],
        "stale_element_token"
    );
    for (key, value) in [("x", json!(10)), ("delivery_mode", json!("foreground"))] {
        let mut invalid = args.clone();
        invalid[key] = value;
        let result = tool.invoke(invalid).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap()["code"],
            "invalid_arguments"
        );
    }
}
