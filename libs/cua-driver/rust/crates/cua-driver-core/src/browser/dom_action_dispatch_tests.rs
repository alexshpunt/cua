//! Native dispatch contracts against a loopback CDP peer, never desktop input.
use super::action_prepare_tests::{request, source};
use super::*;
use crate::browser::dom_action_progress::DomActionProgress;
use crate::browser::engine::DomActionRoute;

#[tokio::test]
async fn trusted_route_never_silently_falls_back_to_programmatic_dom_events() {
    let (f, target, tab, context) = source().await;
    let count = f.state.lock().unwrap().calls.len();
    let progress = DomActionProgress::default();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("click"),
            DomActionRoute::Trusted,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserInputTrustUnavailable);
    assert_eq!(progress.snapshot()["mutation_attempted"], false);
    assert_eq!(f.state.lock().unwrap().calls.len(), count);
}

#[tokio::test]
async fn explicit_dom_route_dispatches_only_fixed_attested_steps_and_retires_readonly_refs() {
    let (f, target, tab, context) = source().await;
    let progress = DomActionProgress::default();
    let result = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("click"),
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "dispatched");
    assert_eq!(result["input_route"], "dom_event");
    assert_eq!(result["trusted"], false);
    assert_eq!(result["effect"], "unverified");
    assert_eq!(progress.snapshot()["cleanup"], "confirmed");
    assert_eq!(progress.snapshot()["acknowledged_steps"], json!(["click"]));
    assert_eq!(
        f.engine
            .store
            .resolve_dom_ref("dom-inspection-tests", &target, &tab, &context)
            .unwrap_err()
            .code,
        BrowserRefusalCode::BrowserRefStale
    );
    let state = f.state.lock().unwrap();
    assert!(!state
        .calls
        .iter()
        .any(|(method, _)| method.starts_with("Input.")
            || method.starts_with("Emulation.")
            || method == "DOM.getBoxModel"));
    assert!(
        state
            .calls
            .iter()
            .filter(|(method, _)| method == "Runtime.releaseObjectGroup")
            .count()
            >= 3
    );
}
