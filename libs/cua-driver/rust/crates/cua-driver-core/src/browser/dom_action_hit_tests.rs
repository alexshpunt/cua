//! Public native action contracts against the owned mock CDP source. No GUI input.
use super::action_prepare_tests::source;
use super::*;
use crate::browser::dom_action_tool::BrowserDomActionTool;

fn command(target: &str, tab: &str, reference: &str) -> Value {
    json!({"session":"dom-inspection-tests","target_id":target,"tab_id":tab,
        "source_ref":reference,"input_route":"dom_event",
        "request":{"action":"click","locator":{"test_id":"live-value"},"timeout_ms":500}})
}

#[tokio::test]
async fn pointer_readiness_uses_document_css_native_hit_identity_and_same_world_descendants() {
    for descendant in [false, true] {
        let (f, target, tab, reference) = source().await;
        if descendant {
            f.state.lock().unwrap().hit_backend = Some(99);
        }
        let result = structured(
            &BrowserDomActionTool::new(f.engine.clone())
                .invoke(command(&target, &tab, &reference))
                .await,
        );
        assert_eq!(result["status"], "dispatched", "{result}");
        assert_eq!(result["progress"]["acknowledged_steps"], json!(["click"]));
        let state = f.state.lock().unwrap();
        assert!(state.hit_reads >= 2);
        assert!(state.actionability_reads >= 3);
        assert!(state
            .calls
            .iter()
            .any(|(method, args)| method == "DOM.getNodeForLocation"
                && args["y"] == 155
                && args["includeUserAgentShadowDOM"] == false
                && args["ignorePointerEventsNone"] == false));
        assert!(!state
            .calls
            .iter()
            .any(|(method, _)| method.starts_with("Input.") || method.starts_with("Emulation.")));
    }
}

#[tokio::test]
async fn unsupported_changed_or_exceptional_hits_preserve_zero_attempts_and_closed_diagnostics() {
    for fault in [
        "frame",
        "ua",
        "pseudo",
        "frame_element",
        "closed",
        "unrelated",
        "stale",
        "foreign_document",
        "changed_hit",
        "changed_sample",
        "eval_error",
        "hit_exception",
        "navigation",
    ] {
        let (f, target, tab, reference) = source().await;
        {
            let mut state = f.state.lock().unwrap();
            match fault {
                "frame" => state.hit_frame = Some("CHILD_FRAME".into()),
                "ua" => state.hit_description_fault = "ua".into(),
                "pseudo" => state.hit_description_fault = "pseudo".into(),
                "frame_element" => state.hit_description_fault = "frame".into(),
                "closed" => state.hit_relation = Some(Value::Null),
                "unrelated" => state.hit_relation = Some(json!(false)),
                "stale" => state.hit_description_fault = "stale".into(),
                "foreign_document" => state.hit_description_fault = "foreign_document".into(),
                "changed_hit" => state.hit_changes = true,
                "changed_sample" => state.actionability_sample_changes = true,
                "eval_error" => state.actionability_eval_error = true,
                "hit_exception" => state.hit_exception = true,
                "navigation" => state.hit_navigates = true,
                _ => unreachable!(),
            }
        }
        let result = structured(
            &BrowserDomActionTool::new(f.engine.clone())
                .invoke(command(&target, &tab, &reference))
                .await,
        );
        assert_eq!(result["status"], "refused", "{fault}: {result}");
        assert_eq!(
            result["progress"]["attempted_steps"],
            json!([]),
            "{fault}: {result}"
        );
        assert_eq!(result["progress"]["acknowledged_steps"], json!([]));
        assert_eq!(result["progress"]["in_flight"], Value::Null);
        assert_eq!(result["progress"]["mutation_attempted"], false);
        assert_eq!(result["progress"]["cleanup"], "not_started");
        let (code, reason) = match fault {
            "frame" => (
                "browser_route_unavailable",
                Some("dom_action_hit_frame_unavailable"),
            ),
            "ua" | "pseudo" | "frame_element" => (
                "browser_route_unavailable",
                Some("dom_action_hit_unavailable"),
            ),
            "closed" => (
                "browser_action_unavailable",
                Some("dom_action_state_unavailable"),
            ),
            "unrelated" => ("browser_action_unavailable", Some("dom_action_timeout")),
            "stale" | "foreign_document" | "navigation" => ("browser_ref_stale", None),
            "changed_hit" => ("browser_route_unavailable", Some("dom_action_hit_changed")),
            "changed_sample" => (
                "browser_route_unavailable",
                Some("dom_action_sample_changed"),
            ),
            "eval_error" | "hit_exception" => (
                "browser_route_unavailable",
                Some("dom_read_call_function_eval_error"),
            ),
            _ => unreachable!(),
        };
        assert_eq!(result["refusal"]["code"], code, "{fault}: {result}");
        if let Some(reason) = reason {
            assert_eq!(result["progress"]["reason"], reason, "{fault}: {result}");
        }
        assert!(!result.to_string().contains("PRIVATE"));
        assert!(cua_driver_contract::DomActionResult::from_value(result.clone()).is_ok());
        let state = f.state.lock().unwrap();
        assert!(state
            .calls
            .iter()
            .any(|(method, _)| method == "Runtime.releaseObjectGroup"));
        assert!(!state
            .calls
            .iter()
            .any(|(method, args)| method.starts_with("Input.")
                || method.starts_with("Emulation.")
                || method == "Runtime.callFunctionOn"
                    && args["functionDeclaration"]
                        == crate::browser::dom_action_operation::STEP_FUNCTION));
    }
}
