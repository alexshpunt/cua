//! Owner tests for the opt-in dynamic native geometry route and source interval.
use super::*;

async fn request(f: &Fixture, target: &str, tab: &str) -> Value {
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(target, tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .clone();
    let mut request = args(target, tab);
    request["native_geometry_ref"] = reference;
    request
}

#[tokio::test]
async fn native_projection_brackets_css_and_native_source_without_authority() {
    let (f, target, tab) = bound().await;
    let request = request(&f, &target, &tab).await;
    let result = structured(
        &GetBrowserStateTool::new(f.engine.clone())
            .invoke(request.clone())
            .await,
    );
    assert_eq!(result["format"], "native_dom_geometry_v1", "{result}");
    assert_eq!(
        result["physical_enclosing_rect"],
        json!({"x":25.0,"y":130.0,"width":121.0,"height":46.0})
    );
    assert_eq!(result["ref"], request["native_geometry_ref"]);
    assert_eq!(result["source"]["frame_id"], "F_MAIN");
    assert_eq!(result["source"]["loader_id"], "L_MAIN");
    assert_eq!(result["source"]["context_unique_id"], "unique-main-77");
    assert_eq!(result["native"]["source"], "uia_root_web_area");
    assert_eq!(
        result["native"]["window"],
        json!({"x":0.0,"y":0.0,"width":1200.0,"height":900.0})
    );
    assert_eq!(
        result["native"]["viewport"],
        json!({"x":10.0,"y":100.0,"width":750.0,"height":450.0})
    );
    assert_eq!(result["css_sample"]["layout_viewport"]["dpr"], 1.5);
    assert_eq!(
        f.engine.platform.native_window(1, 7).await.unwrap().bounds,
        Rect::new(0., 0., 800., 600.),
        "CDP-correlation geometry stays in DIP instead of being fitted to physical pixels"
    );
    assert_eq!(
        result["browser_version"],
        json!({"product":"Chrome/145.0.fixture","revision":"@fixture-revision"})
    );
    assert_eq!(result["css_sample"]["scroll"]["y"], 120.0);
    assert_eq!(result["clipped"], false);
    assert_eq!(result["atomic"], false);
    assert_eq!(result["input_authority"], false);
    assert_eq!(result["qualification"], "candidate_not_raster_qualified");
    let state = f.state.lock().unwrap();
    assert_eq!(state.native_geometry_calls, 2);
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(method, _)| method == "Runtime.callFunctionOn")
            .count(),
        2
    );
    assert!(state
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.releaseObjectGroup"));
    assert!(!state
        .calls
        .iter()
        .any(|(method, _)| method.starts_with("Input.")
            || matches!(
                method.as_str(),
                "DOM.focus"
                    | "DOM.scrollIntoViewIfNeeded"
                    | "Page.captureScreenshot"
                    | "Target.activateTarget"
                    | "Page.bringToFront"
                    | "Emulation.setDeviceMetricsOverride"
            )));
}

#[tokio::test]
async fn native_projection_refuses_source_changes_and_never_falls_back_to_css() {
    for (fault, code) in [
        ("one_pixel", "browser_route_unavailable"),
        ("identity", "browser_ref_stale"),
        ("window", "browser_ref_stale"),
        ("physical_window", "browser_ref_stale"),
        ("css", "browser_ref_stale"),
        ("navigation", "browser_ref_stale"),
        ("unavailable", "browser_route_unavailable"),
    ] {
        let (f, target, tab) = bound().await;
        let request = request(&f, &target, &tab).await;
        f.state.lock().unwrap().native_geometry_fault = fault.into();
        let result = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(request)
                .await,
        );
        assert_eq!(result["refusal"]["code"], code, "{fault}: {result}");
        assert!(result.get("physical_enclosing_rect").is_none(), "{fault}");
        let state = f.state.lock().unwrap();
        if matches!(fault, "window" | "physical_window") {
            assert_eq!(
                state.native_geometry_calls, 2,
                "{fault}: compare both physical samples and retain independent DIP continuity"
            );
        }
        assert!(state
            .calls
            .iter()
            .any(|(method, _)| method == "Runtime.releaseObjectGroup"));
    }
}

#[tokio::test]
async fn native_projection_requires_attested_context_and_current_main_document() {
    for fault in ["reused_id", "missing_unique", "child", "stale", "revoked"] {
        let (f, target, tab) = bound().await;
        let mut request = request(&f, &target, &tab).await;
        match fault {
            "reused_id" => f.state.lock().unwrap().geometry_context_fault = "reused_id".into(),
            "missing_unique" => f.state.lock().unwrap().geometry_missing_unique = true,
            "child" => {
                let snapshot = structured(
                    &GetBrowserStateTool::new(f.engine.clone())
                        .invoke(args(&target, &tab))
                        .await,
                );
                request["native_geometry_ref"] = snapshot["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["frame"]["kind"] == "iframe")
                    .unwrap()["ref"]
                    .clone();
            }
            "stale" => f.state.lock().unwrap().navigated = true,
            "revoked" => f.engine.store.remove_session("dom-inspection-tests"),
            _ => unreachable!(),
        }
        let result = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(request)
                .await,
        );
        assert_eq!(
            result["refusal"]["code"],
            if fault == "stale" {
                "browser_ref_stale"
            } else if fault == "revoked" {
                "browser_binding_stale"
            } else {
                "browser_route_unavailable"
            },
            "{fault}: {result}"
        );
        assert_eq!(f.state.lock().unwrap().native_geometry_calls, 0, "{fault}");
    }
}

#[tokio::test]
async fn native_projection_validation_never_silently_becomes_collection_or_css() {
    let (f, target, tab) = bound().await;
    let request = request(&f, &target, &tab).await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    for (field, value) in [
        ("geometry_ref", request["native_geometry_ref"].clone()),
        ("locator", json!({"test_id":"save"})),
        ("native_geometry_ref", json!("")),
        ("snapshot_format", json!("dom_refs_v1")),
        ("include_screenshot", json!(true)),
    ] {
        let mut invalid = request.clone();
        invalid[field] = value;
        let result = tool.invoke(invalid).await;
        assert_eq!(result.is_error, Some(true), "{field}: {result:?}");
    }
    assert_eq!(f.state.lock().unwrap().native_geometry_calls, 0);
}
