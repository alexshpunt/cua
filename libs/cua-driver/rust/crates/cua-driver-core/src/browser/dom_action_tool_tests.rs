//! Public native adapter contracts, executed against loopback CDP rather than GUI input.
use super::action_prepare_tests::source;
use super::*;
use crate::browser::dom_action_tool::BrowserDomActionTool;

fn wire(target:&str,tab:&str,source_ref:&str)->Value {
    json!({"session":"dom-inspection-tests","target_id":target,"tab_id":tab,
        "source_ref":source_ref,"input_route":"dom_event",
        "request":{"action":"click","locator":{"test_id":"live-value"},"timeout_ms":1000}})
}

#[tokio::test]
async fn the_public_schema_rejects_implicit_routes_coordinates_and_irrelevant_fields() {
    let (f,target,tab,context)=source().await;
    let tool=BrowserDomActionTool::new(f.engine.clone());
    let schema=jsonschema::validator_for(&tool.def().input_schema).unwrap();
    let valid=wire(&target,&tab,&context);
    assert!(schema.is_valid(&valid));
    for mutation in [json!({"input_route":null}),json!({"x":1}),json!({"request":{"action":"click","locator":{"css":"button"},"text":"irrelevant"}}),
        json!({"request":{"action":"hover","locator":{"css":"button"}}}),
        json!({"request":{"action":"fill","locator":{},"text":"x"}})] {
        let mut invalid=valid.clone();
        invalid.as_object_mut().unwrap().extend(mutation.as_object().unwrap().clone());
        assert!(!schema.is_valid(&invalid),"{invalid}");
        let count=f.state.lock().unwrap().calls.len();
        let result=structured(&tool.invoke(invalid).await);
        assert_eq!(result["status"],"refused");
        assert_eq!(result["refusal"]["code"],"browser_action_unavailable");
        assert_eq!(result["refusal"]["message"],"dom_action_invalid_request");
        assert_eq!(f.state.lock().unwrap().calls.len(),count);
    }
}

#[tokio::test]
async fn explicit_public_dispatch_preserves_prefixes_and_never_claims_application_success() {
    let (f,target,tab,context)=source().await;
    let tool=BrowserDomActionTool::new(f.engine.clone());
    let result=tool.invoke(wire(&target,&tab,&context)).await;
    let record=result.action_record.as_ref().expect("internal action accounting");
    assert_eq!(record.effect,crate::action_record::ActionEffect::Unverifiable);
    let content=structured(&result);
    assert_eq!(content["format"],"dom_action_v1");
    assert_eq!(content["status"],"dispatched");
    assert_eq!(content["progress"]["acknowledged_steps"],json!(["click"]));
    assert_eq!(content["action_result"]["effect"],"unverifiable");
    assert_eq!(content["action_result"]["route"],"dom");
    assert_eq!(content["trusted"],false);
    assert_eq!(content["atomic"],false);
    assert_eq!(content["progress"]["cleanup"],"confirmed");
}

#[tokio::test]
async fn the_public_trusted_request_refuses_before_lookup_and_never_downgrades() {
    let (f,target,tab,context)=source().await;
    let tool=BrowserDomActionTool::new(f.engine.clone());
    let mut args=wire(&target,&tab,&context);args["input_route"]=json!("trusted");
    let count=f.state.lock().unwrap().calls.len();
    let result=structured(&tool.invoke(args).await);
    assert_eq!(result["status"],"refused");
    assert_eq!(result["refusal"]["code"],"browser_input_trust_unavailable");
    assert_eq!(result["progress"]["mutation_attempted"],false);
    assert_eq!(f.state.lock().unwrap().calls.len(),count);
}

#[tokio::test]
async fn a_public_unknown_dispatch_returns_the_native_attempted_prefix_without_replay() {
    let (f,target,tab,context)=source().await;
    f.state.lock().unwrap().dispatch_fail_step="dblclick".into();
    let tool=BrowserDomActionTool::new(f.engine.clone());
    let mut args=wire(&target,&tab,&context);args["request"]["action"]=json!("double_click");
    let result=tool.invoke(args).await;
    assert_eq!(result.action_record.as_ref().unwrap().effect,crate::action_record::ActionEffect::Unverifiable);
    let result=structured(&result);
    assert_eq!(result["status"],"refused");
    assert_eq!(result["progress"]["acknowledged_steps"],json!(["click","click"]));
    assert_eq!(result["progress"]["in_flight"],"dblclick");
    assert_eq!(result["action_result"]["effect"],"unverifiable");
}

#[tokio::test]
async fn public_dom_actions_use_the_existing_exact_browser_resource_scope() {
    let (f,target,tab,context)=source().await;
    let tool=BrowserDomActionTool::new(f.engine.clone());
    let args=wire(&target,&tab,&context);
    assert_eq!(tool.protected_resource_ownership("browser_bound_input",&args).await,
        crate::tool::ProtectedResourceOwnership::UserOwned);
    let scope=tool.protected_resource_scope("browser_bound_input",&args).await.unwrap().unwrap();
    assert_eq!(scope["kind"],"authenticated_browser_tab");
    assert_eq!(scope["target_id"],target);
    assert_eq!(scope["tab_id"],tab);
    assert_eq!(scope["action_class"],"page_input");
    assert_eq!(scope["live_origin"],"https://fixture.test");
    assert!(!f.state.lock().unwrap().calls.iter().any(|(method,_)|method.starts_with("Input.")));
}
