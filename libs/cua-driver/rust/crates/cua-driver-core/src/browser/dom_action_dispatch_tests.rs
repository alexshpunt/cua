//! Native dispatch contracts against a loopback CDP peer, never desktop input.
use super::action_prepare_tests::{request, source};
use super::*;
use crate::browser::dom_action_progress::DomActionProgress;
use crate::browser::dom_action_request::parse_request;
use crate::browser::engine::DomActionRoute;

#[tokio::test]
async fn form_ua_internals_allow_preparation_but_author_gaps_and_duplicates_still_refuse() {
    for fault in ["none", "closed", "missing_children", "duplicate"] {
        let (f, target, tab, context) = source().await;
        let (mut document, mut layout) = author_form_fixture();
        match fault {
            "closed" => document["root"]["children"][0]["shadowRoots"]
                .as_array_mut()
                .unwrap()
                .push(json!({"nodeType":11,"nodeId":301,"backendNodeId":301,
                    "shadowRootType":"closed","childNodeCount":0,"children":[]})),
            "missing_children" => {
                document["root"]["children"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("childNodeCount");
            }
            "duplicate" => {
                let mut duplicate = document["root"]["children"][0].clone();
                duplicate["nodeId"] = json!(17);
                duplicate["backendNodeId"] = json!(17);
                document["root"]["children"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
                document["root"]["childNodeCount"] = json!(5);
                layout["documents"][0]["nodes"]["backendNodeId"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(17));
                layout["documents"][0]["layout"]["nodeIndex"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(8));
                layout["documents"][0]["layout"]["styles"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!([1, 2, 3]));
            }
            _ => {}
        }
        {
            let mut state = f.state.lock().unwrap();
            state.dom_document = Some(document);
            state.dom_layout = Some(layout);
            state.actionability_reply = Some(json!({"result":{"type":"object","value":{
                "connected":true,"same_document":true,"bounds":[10.5,20.25,80.0,30.0],
                "visible":true,"enabled":true,"editable":true,"receives_pointer":null,"hit_test":{"x":50,"y":155,"scroll_x":0.0,"scroll_y":120.0,"width":800.0,"height":600.0}
            }}}));
        }
        let request = parse_request(
            &json!({"action":"fill","locator":{"test_id":"live-value"},"text":"Ada"}),
        )
        .unwrap();
        let progress = DomActionProgress::default();
        let result = f
            .engine
            .dispatch_dom_action(
                "dom-inspection-tests",
                &target,
                &tab,
                &context,
                &request,
                DomActionRoute::DomEvent,
                progress.clone(),
            )
            .await;
        if fault == "none" {
            let result = result.unwrap();
            assert_eq!(result["status"], "dispatched");
            assert_eq!(
                progress.snapshot()["acknowledged_steps"],
                json!(["focus", "beforeinput", "fill", "input"])
            );
            assert_eq!(
                dispatched_steps(&f.state.lock().unwrap()),
                ["focus", "beforeinput", "fill", "input"]
            );
        } else {
            let error = result.unwrap_err();
            let reason = if fault == "duplicate" {
                "dom_action_ambiguous"
            } else {
                "dom_action_coverage_incomplete"
            };
            assert_eq!(
                error.code,
                BrowserRefusalCode::BrowserActionUnavailable,
                "{fault}"
            );
            assert_eq!(error.message, reason, "{fault}");
            assert_eq!(progress.snapshot()["reason"], reason, "{fault}");
            assert_eq!(progress.snapshot()["mutation_attempted"], false, "{fault}");
            assert!(
                dispatched_steps(&f.state.lock().unwrap()).is_empty(),
                "{fault}"
            );
        }
    }
}

#[tokio::test]
async fn an_unknown_normal_keyup_is_never_replayed_as_cleanup() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_fail_step = "keyup".into();
    let request=parse_request(&json!({"action":"press","locator":{"test_id":"live-value"},"key":"Enter","timeout_ms":1000})).unwrap();
    let progress = DomActionProgress::default();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request,
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserRouteUnavailable);
    assert_eq!(
        progress.snapshot()["acknowledged_steps"],
        json!(["focus", "keydown"])
    );
    assert_eq!(progress.snapshot()["in_flight"], "keyup");
    assert_eq!(
        dispatched_steps(&f.state.lock().unwrap()),
        ["focus", "keydown", "keyup"]
    );
}
#[tokio::test]
async fn known_native_refusal_acknowledges_only_the_completed_prefix() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_negative_step = "beforeinput".into();
    f.state.lock().unwrap().actionability_reply = Some(json!({"result":{"type":"object","value":{
        "connected":true,"same_document":true,"bounds":[10.5,20.25,80.0,30.0],
        "visible":true,"enabled":true,"editable":true,"receives_pointer":null,"hit_test":{"x":50,"y":155,"scroll_x":0.0,"scroll_y":120.0,"width":800.0,"height":600.0}
    }}}));
    let request = parse_request(
        &json!({"action":"fill","locator":{"test_id":"live-value"},"text":"private"}),
    )
    .unwrap();
    let progress = DomActionProgress::default();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request,
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    assert_eq!(error.message, "dom_action_event_cancelled");
    assert_eq!(
        progress.snapshot()["acknowledged_steps"],
        json!(["focus", "beforeinput"])
    );
    assert_eq!(progress.snapshot()["in_flight"], Value::Null);
    assert_eq!(
        dispatched_steps(&f.state.lock().unwrap()),
        ["focus", "beforeinput"]
    );
}

#[tokio::test]
async fn the_mutating_world_needs_its_own_unique_context_evidence() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_missing_unique = true;
    let progress = DomActionProgress::default();
    let error = f
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
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserRouteUnavailable);
    assert_eq!(
        error.message,
        "isolated context lacks a bounded unique identity"
    );
    assert_eq!(progress.snapshot()["mutation_attempted"], false);
    assert_eq!(progress.snapshot()["cleanup"], "confirmed");
    assert!(dispatched_steps(&f.state.lock().unwrap()).is_empty());
}

#[tokio::test]
async fn cleanup_failure_is_not_a_successful_action_receipt() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_cleanup_fails = true;
    f.state.lock().unwrap().distinct_attachment_ids = true;
    let progress = DomActionProgress::default();
    let error = f
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
        .unwrap_err();
    assert_eq!(error.message, "dom_action_cleanup_unconfirmed");
    assert_eq!(progress.snapshot()["acknowledged_steps"], json!(["click"]));
    assert_eq!(progress.snapshot()["cleanup"], "unconfirmed");
    assert_ne!(progress.snapshot()["status"], "dispatched");
}

#[tokio::test]
async fn one_request_deadline_includes_hover_dwell_and_requests_owned_cleanup() {
    let (f, target, tab, context) = source().await;
    let released = Arc::new(tokio::sync::Notify::new());
    f.state.lock().unwrap().dispatch_released = Some(released.clone());
    let request=parse_request(&json!({"action":"hover","locator":{"test_id":"live-value"},"dwell_ms":5000,"timeout_ms":2000})).unwrap();
    let progress = DomActionProgress::default();
    let started = std::time::Instant::now();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request,
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "dom_action_timeout");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(progress.snapshot()["status"], "timeout");
    assert_eq!(
        progress.snapshot()["acknowledged_steps"],
        json!(["pointerover", "mouseover", "pointerenter", "mouseenter"])
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), released.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn cancelled_key_cleanup_keeps_same_target_serialization_until_release() {
    let (f, target, tab, context) = source().await;
    let sent = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = f.state.lock().unwrap();
        state.dispatch_signal = Some(sent.clone());
        state.dispatch_signal_step = "keydown".into();
        state.dispatch_stall_step = "keyup".into();
        state.dispatch_released = Some(released.clone());
    }
    let request=parse_request(&json!({"action":"press","locator":{"test_id":"live-value"},"key":"Enter","timeout_ms":1000})).unwrap();
    let progress = DomActionProgress::default();
    {
        let operation = f.engine.dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request,
            DomActionRoute::DomEvent,
            progress.clone(),
        );
        tokio::pin!(operation);
        tokio::select! {biased;
            _=sent.notified()=>{},
            _=&mut operation=>panic!("operation completed before keydown seam"),
        }
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            f.engine
                .lock_mutation("dom-inspection-tests", &target, &tab)
        )
        .await
        .is_err(),
        "another action must not interleave with the owned cleanup key-up"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), released.notified())
        .await
        .unwrap();
    let _next = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        f.engine
            .lock_mutation("dom-inspection-tests", &target, &tab),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(progress.snapshot()["status"], "cancelled");
    assert_eq!(
        dispatched_steps(&f.state.lock().unwrap()),
        ["focus", "keydown", "keyup"]
    );
}

fn dispatched_steps(state: &State) -> Vec<String> {
    state
        .calls
        .iter()
        .filter(|(method, params)| {
            method == "Runtime.callFunctionOn"
                && params["functionDeclaration"]
                    == crate::browser::dom_action_operation::STEP_FUNCTION
        })
        .map(|(_, params)| {
            params["arguments"][1]["value"]["name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn every_declared_action_uses_the_closed_plan_without_coordinate_or_input_calls() {
    for data in [
        json!({"action":"double_click"}),
        json!({"action":"right_click"}),
        json!({"action":"hover","dwell_ms":1}),
        json!({"action":"fill","text":"private"}),
        json!({"action":"type","text":"private"}),
        json!({"action":"press","key":"Control+A"}),
        json!({"action":"check"}),
        json!({"action":"uncheck"}),
        json!({"action":"select_option","values":["private"]}),
        json!({"action":"focus"}),
        json!({"action":"scroll_into_view"}),
    ] {
        let (f, target, tab, context) = source().await;
        f.state.lock().unwrap().actionability_reply = Some(
            json!({"result":{"type":"object","value":{
                "connected":true,"same_document":true,"bounds":[10.5,20.25,80.0,30.0],
                "visible":true,"enabled":true,"editable":true,"receives_pointer":null,"hit_test":{"x":50,"y":155,"scroll_x":0.0,"scroll_y":120.0,"width":800.0,"height":600.0}
            }}}),
        );
        let mut data = data;
        data["locator"] = json!({"test_id":"live-value"});
        data["timeout_ms"] = json!(1000);
        let request = parse_request(&data).unwrap();
        let progress = DomActionProgress::default();
        let result = f
            .engine
            .dispatch_dom_action(
                "dom-inspection-tests",
                &target,
                &tab,
                &context,
                &request,
                DomActionRoute::DomEvent,
                progress.clone(),
            )
            .await
            .unwrap();
        assert_eq!(result["status"], "dispatched", "{data}");
        assert!(result["observed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| sample["observed"] == json!({"focused":true})));
        assert!(!progress.snapshot().to_string().contains("private"));
        let state = f.state.lock().unwrap();
        assert!(!dispatched_steps(&state).is_empty());
        assert!(!state
            .calls
            .iter()
            .any(|(method, _)| method.starts_with("Input.")
                || method.starts_with("Emulation.")
                || matches!(
                    method.as_str(),
                    "DOM.getBoxModel" | "DOM.scrollIntoViewIfNeeded" | "Page.bringToFront"
                )));
    }
}

#[tokio::test]
async fn unknown_second_effect_preserves_acknowledged_prefix_and_never_replays_or_continues() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_fail_step = "dblclick".into();
    let progress = DomActionProgress::default();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("double_click"),
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserRouteUnavailable);
    let receipt = progress.snapshot();
    assert_eq!(
        receipt["attempted_steps"],
        json!(["click", "click", "dblclick"])
    );
    assert_eq!(receipt["acknowledged_steps"], json!(["click", "click"]));
    assert_eq!(receipt["in_flight"], "dblclick");
    assert_eq!(receipt["status"], "unknown");
    assert_eq!(receipt["cleanup"], "confirmed");
    assert_eq!(
        dispatched_steps(&f.state.lock().unwrap()),
        ["click", "click", "dblclick"]
    );
}

#[tokio::test]
async fn navigation_after_first_effect_stops_before_any_second_locator_action() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_navigate_step = "click".into();
    let progress = DomActionProgress::default();
    let error = f
        .engine
        .dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request("double_click"),
            DomActionRoute::DomEvent,
            progress.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserRefStale);
    assert_eq!(progress.snapshot()["acknowledged_steps"], json!(["click"]));
    assert_eq!(dispatched_steps(&f.state.lock().unwrap()), ["click"]);
}

#[tokio::test]
async fn unsupported_control_preflight_has_no_mutation_prefix_and_releases_handles() {
    let (f, target, tab, context) = source().await;
    f.state.lock().unwrap().dispatch_preflight_refused = true;
    let progress = DomActionProgress::default();
    let error = f
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
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserActionUnavailable);
    assert_eq!(error.message, "dom_action_operation_refused");
    assert_eq!(progress.snapshot()["mutation_attempted"], false);
    assert_eq!(progress.snapshot()["cleanup"], "confirmed");
    assert!(dispatched_steps(&f.state.lock().unwrap()).is_empty());
}

#[tokio::test]
async fn cancelling_in_flight_dispatch_retains_unknown_prefix_and_owned_cleanup() {
    let (f, target, tab, context) = source().await;
    let sent = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = f.state.lock().unwrap();
        state.dispatch_signal = Some(sent.clone());
    }
    let request = request("click");
    let progress = DomActionProgress::default();
    {
        let operation = f.engine.dispatch_dom_action(
            "dom-inspection-tests",
            &target,
            &tab,
            &context,
            &request,
            DomActionRoute::DomEvent,
            progress.clone(),
        );
        tokio::pin!(operation);
        tokio::select! {biased;
            _=sent.notified()=>{},
            _=&mut operation=>panic!("dispatch completed before its cancellation seam"),
        }
        f.state.lock().unwrap().geometry_released = Some(released.clone());
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), released.notified())
        .await
        .expect("owned group release");
    assert_eq!(progress.snapshot()["status"], "cancelled");
    assert_eq!(progress.snapshot()["attempted_steps"], json!(["click"]));
    assert_eq!(progress.snapshot()["in_flight"], "click");
    assert_eq!(dispatched_steps(&f.state.lock().unwrap()), ["click"]);
}

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
