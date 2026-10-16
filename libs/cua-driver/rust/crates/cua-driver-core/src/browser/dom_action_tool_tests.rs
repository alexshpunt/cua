//! Public native adapter contracts, executed against loopback CDP rather than GUI input.
use super::action_prepare_tests::source;
use super::*;
use crate::browser::dom_action_tool::BrowserDomActionTool;
struct Approval(std::sync::atomic::AtomicUsize);
#[async_trait]
impl crate::consent::ProtectedConsentProvider for Approval {
    fn provider_id(&self) -> &'static str {
        "test.dom-action-admission"
    }
    async fn request_consent(
        &self,
        request: &crate::consent::ConsentRequest,
    ) -> Result<crate::consent::ProviderDecision, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::consent::ProviderDecision {
            action: crate::consent::ConsentAction::Accept,
            request_digest: request.request_digest.clone(),
        })
    }
}

#[tokio::test]
async fn a_unicode_native_failure_stays_inside_the_advertised_receipt_budget() {
    let (f, _, tab, context) = source().await;
    let tool = BrowserDomActionTool::new(f.engine.clone());
    let result = structured(&tool.invoke(wire(&"😀".repeat(256), &tab, &context)).await);
    assert_eq!(result["status"], "refused");
    assert_eq!(result["progress"]["mutation_attempted"], false);
    assert!(
        cua_driver_contract::DomActionResult::from_value(result.clone()).is_ok(),
        "{result}"
    );
}
fn wire(target: &str, tab: &str, source_ref: &str) -> Value {
    json!({"session":"dom-inspection-tests","target_id":target,"tab_id":tab,
        "source_ref":source_ref,"input_route":"dom_event",
        "request":{"action":"click","locator":{"test_id":"live-value"},"timeout_ms":5000}})
}

#[tokio::test]
async fn the_public_schema_rejects_implicit_routes_coordinates_and_irrelevant_fields() {
    let (f, target, tab, context) = source().await;
    let tool = BrowserDomActionTool::new(f.engine.clone());
    let schema = jsonschema::validator_for(&tool.def().input_schema).unwrap();
    let valid = wire(&target, &tab, &context);
    assert!(schema.is_valid(&valid));
    for mutation in [
        json!({"input_route":null}),
        json!({"session":"s".repeat(257)}),
        json!({"x":1}),
        json!({"request":{"action":"click","locator":{"css":"button"},"text":"irrelevant"}}),
        json!({"request":{"action":"hover","locator":{"css":"button"}}}),
        json!({"request":{"action":"fill","locator":{},"text":"x"}}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(mutation.as_object().unwrap().clone());
        assert!(!schema.is_valid(&invalid), "{invalid}");
        let count = f.state.lock().unwrap().calls.len();
        let result = structured(&tool.invoke(invalid).await);
        assert_eq!(result["status"], "refused");
        assert_eq!(result["refusal"]["code"], "browser_action_unavailable");
        assert_eq!(result["refusal"]["message"], "dom_action_invalid_request");
        assert_eq!(f.state.lock().unwrap().calls.len(), count);
    }
}

#[tokio::test]
async fn registry_and_mcp_preserve_success_and_unknown_prefixes_from_the_actual_native_adapter() {
    use crate::authorization::PermissionMode;
    use crate::session_authorization::{SessionAuthorizationRegistry, SessionModeCeiling};
    use std::time::Duration;
    for action in ["click", "double_click"] {
        let (f, _, _, _) = source().await;
        let context = SessionAuthorizationRegistry::with_ceiling(
            SessionModeCeiling::for_trusted_sessions(
                [PermissionMode::Standard],
                false,
                Duration::from_secs(60),
                Duration::from_secs(30),
            )
            .unwrap(),
        )
        .compatibility_context(PermissionMode::Standard, None)
        .unwrap();
        let internal = context.runtime_session_key("dom-inspection-tests");
        let bound = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(json!({"pid":1,"window_id":7,"session":internal}))
                .await,
        );
        let target = bound["target_id"].as_str().unwrap();
        let tab = bound["tabs"][0]["tab_id"].as_str().unwrap();
        let mut read = args(target, tab);
        read["session"] = json!(internal);
        let snapshot = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(read)
                .await,
        );
        let source = snapshot["elements"][0]["ref"].as_str().unwrap();
        let approval = Arc::new(Approval(std::sync::atomic::AtomicUsize::new(0)));
        let mut registry =
            crate::tool::ToolRegistry::new_with_protected_consent_provider(Some(approval.clone()));
        crate::browser::tools::register_browser_tools(&f.engine, &mut registry);
        let discovery = registry.tools_list();
        let entry = discovery["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "browser_dom_action")
            .unwrap();
        assert_eq!(entry["annotations"]["readOnlyHint"], false);
        assert!(
            entry.get("outputSchema").is_some(),
            "MCP must advertise the prefix-preserving receipt"
        );
        if action == "double_click" {
            f.state.lock().unwrap().dispatch_fail_step = "dblclick".into();
        }
        let mut command = wire(target, tab, source);
        command["request"]["action"] = json!(action);
        let result = registry
            .invoke_with_context("browser_dom_action", command, context)
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(
            result.action_record.as_ref().unwrap().effect,
            crate::action_record::ActionEffect::Unverifiable
        );
        let output = crate::mcp_result::conforming_tool_result(
            "browser_dom_action",
            serde_json::to_value(&result).unwrap(),
        );
        let content = &output["structuredContent"];
        assert_ne!(output["isError"], true, "{output}");
        assert_eq!(content["format"], "dom_action_v1");
        assert_eq!(content["action_result"]["effect"], "unverifiable");
        assert_eq!(content["action_result"]["route"], "dom");
        assert_eq!(content["trusted"], false);
        assert_eq!(content["atomic"], false);
        assert_eq!(content["progress"]["cleanup"], "confirmed");
        if action == "click" {
            assert_eq!(content["status"], "dispatched");
            assert_eq!(content["progress"]["acknowledged_steps"], json!(["click"]));
        } else {
            assert_eq!(content["status"], "refused");
            assert_eq!(
                content["progress"]["acknowledged_steps"],
                json!(["click", "click"])
            );
            assert_eq!(content["progress"]["in_flight"], "dblclick");
        }
        // Existing Standard/Routine browser admission does not add a new consent prompt.
        assert_eq!(approval.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn the_public_trusted_request_refuses_before_lookup_and_never_downgrades() {
    let (f, target, tab, context) = source().await;
    let tool = BrowserDomActionTool::new(f.engine.clone());
    let mut args = wire(&target, &tab, &context);
    args["input_route"] = json!("trusted");
    let count = f.state.lock().unwrap().calls.len();
    let result = structured(&tool.invoke(args).await);
    assert_eq!(result["status"], "refused");
    assert_eq!(result["refusal"]["code"], "browser_input_trust_unavailable");
    assert_eq!(result["progress"]["mutation_attempted"], false);
    assert_eq!(f.state.lock().unwrap().calls.len(), count);
}

#[tokio::test]
async fn public_dom_actions_use_the_existing_exact_browser_resource_scope() {
    let (f, target, tab, context) = source().await;
    let tool = BrowserDomActionTool::new(f.engine.clone());
    let args = wire(&target, &tab, &context);
    assert_eq!(
        tool.protected_resource_ownership("browser_bound_input", &args)
            .await,
        crate::tool::ProtectedResourceOwnership::UserOwned
    );
    let scope = tool
        .protected_resource_scope("browser_bound_input", &args)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scope["kind"], "authenticated_browser_tab");
    assert_eq!(scope["target_id"], target);
    assert_eq!(scope["tab_id"], tab);
    assert_eq!(scope["action_class"], "page_input");
    assert_eq!(scope["live_origin"], "https://fixture.test");
    assert!(!f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|(method, _)| method.starts_with("Input.")));
}
