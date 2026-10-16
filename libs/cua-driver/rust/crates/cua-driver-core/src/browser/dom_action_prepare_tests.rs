//! Internal readonly readiness through the real engine/CDP boundary, never GUI input.
use super::*;
use crate::browser::dom_action_request::parse_request;

async fn source() -> (Fixture, String, String, String) {
    let (fixture, target, tab) = bound().await;
    fixture.state.lock().unwrap().single_frame = true;
    let snapshot = structured(
        &GetBrowserStateTool::new(fixture.engine.clone())
            .invoke(args(&target, &tab))
            .await,
    );
    assert_eq!(snapshot["snapshot"]["collection_complete"], true);
    let reference = snapshot["elements"][0]["ref"].as_str().unwrap().to_owned();
    (fixture, target, tab, reference)
}
fn request(action: &str) -> crate::browser::dom_action_request::DomActionRequest {
    parse_request(&json!({"action":action,"locator":{"test_id":"live-value"},"timeout_ms":1000}))
        .unwrap()
}
fn assert_no_input(state: &State) {
    assert!(!state
        .calls
        .iter()
        .any(|(method, _)| method.starts_with("Input.")
            || method.starts_with("Emulation.")
            || matches!(
                method.as_str(),
                "DOM.focus" | "DOM.scrollIntoViewIfNeeded" | "Page.bringToFront"
            )));
}

#[tokio::test]
async fn prepared_locator_is_stable_but_retains_only_readonly_reference_authority() {
    let (f, target, tab, context) = source().await;
    let prepared = f
        .engine
        .prepare_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("click"),
        )
        .await
        .unwrap();
    assert_eq!(prepared.state.target.backend_node_id, 12);
    assert_eq!(prepared.state.target.frame_id, "F_MAIN");
    assert_eq!(prepared.state.target.loader_id, "L_MAIN");
    assert!(prepared.press_key.is_none());
    let reference = f
        .engine
        .store
        .resolve_dom_ref("dom-inspection-tests", &target, &tab, &prepared.reference)
        .unwrap();
    assert!(reference.actions.is_empty());
    let result = structured(&BrowserClickTool::new(f.engine.clone()).invoke(json!({
        "session":"dom-inspection-tests","target_id":target,"tab_id":tab,"ref":prepared.reference
    })).await);
    assert_eq!(result["refusal"]["code"], "browser_action_unavailable");
    let state = f.state.lock().unwrap();
    assert!(
        state
            .calls
            .iter()
            .filter(|(method, _)| method == "Runtime.releaseObjectGroup")
            .count()
            >= 2
    );
    assert_no_input(&state);
}

#[tokio::test]
async fn unsupported_key_is_rejected_before_lookup_or_native_context_creation() {
    let (f, target, tab, context) = source().await;
    let count = f.state.lock().unwrap().calls.len();
    let request = parse_request(
        &json!({"action":"press","locator":{"test_id":"live-value"},"key":"Shift+1"}),
    )
    .unwrap();
    let error = f
        .engine
        .prepare_dom_action("dom-inspection-tests", &target, &tab, &context, &request)
        .await
        .err()
        .expect("unsupported key");
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    assert_eq!(
        error.detail.unwrap()["reason"],
        "dom_action_key_unavailable"
    );
    assert_eq!(f.state.lock().unwrap().calls.len(), count);
}

#[tokio::test]
async fn lookup_cannot_move_the_request_into_a_new_document_with_the_same_url() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().navigated = true;
    let error = f
        .engine
        .prepare_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("click"),
        )
        .await
        .err()
        .expect("changed loader");
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    assert_eq!(error.detail.unwrap()["reason"], "dom_action_source_changed");
    assert_no_input(&f.state.lock().unwrap());
}

#[tokio::test]
async fn partial_metadata_never_becomes_unique_action_readiness() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().bad_metadata = true;
    let error = f
        .engine
        .prepare_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("click"),
        )
        .await
        .err()
        .expect("incomplete lookup");
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    assert_eq!(
        error.detail.unwrap()["reason"],
        "dom_action_coverage_incomplete"
    );
    let state = f.state.lock().unwrap();
    assert!(!state
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.callFunctionOn"));
    assert_no_input(&state);
}

#[tokio::test]
async fn a_hidden_candidate_expires_with_an_explicit_no_mutation_prefix() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().actionability_reply = Some(json!({"result":{"type":"object","value":{
        "connected":true,"same_document":true,"bounds":[10.5,20.25,80.0,30.0],
        "visible":false,"enabled":true,"editable":false,"receives_pointer":true
    }}}));
    let mut request = request("click");
    request.timeout_ms = 200;
    let error = f
        .engine
        .prepare_dom_action("dom-inspection-tests", &target, &tab, &context, &request)
        .await
        .err()
        .expect("timeout");
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    let detail = error.detail.unwrap();
    assert_eq!(detail["reason"], "dom_action_timeout");
    assert_eq!(detail["mutation_attempted"], false);
    assert_eq!(detail["automatic_retry"], false);
    assert_no_input(&f.state.lock().unwrap());
}

#[tokio::test]
async fn cancelling_readiness_after_resolution_still_releases_the_exact_owned_group() {
    let (f, target, tab, context) = source().await;
    let resolved = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = f.state.lock().unwrap();
        state.geometry_resolved = Some(resolved.clone());
        state.geometry_released = Some(released.clone());
    }
    let request = request("click");
    {
        let operation =
            f.engine
                .prepare_dom_action("dom-inspection-tests", &target, &tab, &context, &request);
        tokio::pin!(operation);
        tokio::select! { biased;
            _ = resolved.notified() => {},
            _ = &mut operation => panic!("readiness completed before the cancellation seam"),
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), released.notified())
        .await
        .expect("owned cleanup requested");
    let state = f.state.lock().unwrap();
    let groups = state
        .calls
        .iter()
        .filter(|(method, _)| method == "DOM.resolveNode")
        .map(|(_, params)| params["objectGroup"].as_str().unwrap())
        .collect::<Vec<_>>();
    let released_groups = state
        .calls
        .iter()
        .filter(|(method, _)| method == "Runtime.releaseObjectGroup")
        .map(|(_, params)| params["objectGroup"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(groups, released_groups);
    assert_no_input(&state);
}
