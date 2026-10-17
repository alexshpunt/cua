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
async fn each_dispatch_step_requires_fresh_attested_hits_without_replaying_a_completed_prefix() {
    for fault in ["none", "before_first", "after_first", "changed_after_first"] {
        let (f, target, tab, reference) = source().await;
        f.state.lock().unwrap().dispatch_hit_fault = fault.into();
        let mut args = command(&target, &tab, &reference);
        args["request"]["action"] = json!("double_click");
        args["request"]["timeout_ms"] = json!(2000);
        let result = structured(
            &BrowserDomActionTool::new(f.engine.clone())
                .invoke(args)
                .await,
        );
        let state = f.state.lock().unwrap();
        let steps: Vec<&str> = state
            .calls
            .iter()
            .filter_map(|(method, args)| {
                (method == "Runtime.callFunctionOn"
                    && args["functionDeclaration"]
                        == crate::browser::dom_action_operation::STEP_FUNCTION)
                    .then(|| args["arguments"][1]["value"]["name"].as_str().unwrap())
            })
            .collect();
        if fault == "none" {
            assert_eq!(result["status"], "dispatched", "{result}");
            assert_eq!(steps, ["click", "click", "dblclick"]);
            // Each effect must have its own two-hit/three-sample bracket,
            // not just reuse the earlier preparation evidence.
            let mut hits = 0;
            let mut samples = 0;
            for (method, args) in &state.calls {
                if method == "Runtime.callFunctionOn"
                    && args["functionDeclaration"]
                        == crate::browser::dom_action_operation::PREFLIGHT_FUNCTION
                {
                    hits = 0;
                    samples = 0;
                } else if method == "DOM.getNodeForLocation" {
                    hits += 1;
                } else if method == "Runtime.callFunctionOn"
                    && args["functionDeclaration"]
                        == crate::browser::dom_action_sample::READ_FUNCTION
                {
                    samples += 1;
                } else if method == "Runtime.callFunctionOn"
                    && args["functionDeclaration"]
                        == crate::browser::dom_action_operation::STEP_FUNCTION
                {
                    assert_eq!(hits, 2);
                    assert_eq!(samples, 3);
                    hits = 0;
                    samples = 0;
                }
            }
        } else {
            assert_eq!(result["status"], "refused", "{fault}: {result}");
            let prefix = if fault == "before_first" {
                json!([])
            } else {
                json!(["click"])
            };
            assert_eq!(
                result["progress"]["attempted_steps"], prefix,
                "{fault}: {result}"
            );
            assert_eq!(result["progress"]["acknowledged_steps"], prefix);
            assert_eq!(result["progress"]["in_flight"], Value::Null);
            assert_eq!(result["progress"]["cleanup"], "confirmed");
            assert_eq!(steps.len(), if fault == "before_first" { 0 } else { 1 });
            assert_eq!(
                result["refusal"]["code"],
                if fault == "changed_after_first" {
                    "browser_route_unavailable"
                } else {
                    "browser_action_unavailable"
                }
            );
            assert_eq!(
                result["refusal"]["message"],
                if fault == "changed_after_first" {
                    "dom_action_hit_changed"
                } else {
                    "dom_action_readiness_changed"
                }
            );
        }
        assert!(cua_driver_contract::DomActionResult::from_value(result).is_ok());
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
