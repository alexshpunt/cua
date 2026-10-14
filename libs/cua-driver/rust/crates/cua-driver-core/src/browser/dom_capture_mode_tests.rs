//! Capture routes against a deterministic CDP boundary: no real browser/input.
use super::{BrowserEngine, BrowserScreenshotMode};
use crate::browser::mock_cdp::{MockCdpServer, MockReply};
use crate::browser::platform::{BrowserPlatform, PrepareOutcome, PrepareRequest};
use crate::browser::refusal::{BrowserRefusal, BrowserRefusalCode};
use crate::browser::tools::GetBrowserStateTool;
use crate::browser::types::*;
use crate::tool::Tool;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::ImageEncoder;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Platform {
    endpoint: String,
    gone: Arc<Mutex<bool>>,
}
#[async_trait]
impl BrowserPlatform for Platform {
    async fn classify_browser(&self, _: i64) -> Result<BrowserClassification, BrowserRefusal> {
        Ok(BrowserClassification {
            is_browser: true,
            engine: BrowserEngineFamily::Chromium,
            product_kind: BrowserProduct::Electron,
            product: None,
            channel: None,
            process_role: BrowserProcessRole::EmbeddedApplication,
            supports_cdp: true,
        })
    }
    async fn native_window(
        &self,
        pid: i64,
        window_id: u64,
    ) -> Result<NativeWindowInfo, BrowserRefusal> {
        if *self.gone.lock().unwrap() {
            return Err(BrowserRefusal::new(
                BrowserRefusalCode::BrowserWrongTargetRefused,
                "owned source closed",
            ));
        }
        Ok(NativeWindowInfo {
            pid,
            window_id,
            title: "Capture fixture".into(),
            bounds: Rect::new(0., 0., 800., 600.),
            geometry_exact: true,
            ownership: NativeOwnershipProof {
                method: NativeOwnershipMethod::WindowServerOwner,
                owner_pid: pid,
                detail: None,
            },
        })
    }
    async fn is_only_exact_native_window(
        &self,
        _: i64,
        _: u64,
    ) -> Result<Option<bool>, BrowserRefusal> {
        Ok(Some(true))
    }
    async fn discover_owned_endpoint(
        &self,
        pid: i64,
    ) -> Result<Option<OwnedEndpoint>, BrowserRefusal> {
        Ok(Some(OwnedEndpoint {
            ws_url: self.endpoint.clone(),
            http_port: None,
            transport: EndpointTransport::EmbeddedDescendant,
            ownership: EndpointOwnershipProof {
                method: EndpointOwnershipMethod::ListeningSocketPid,
                owner_pid: pid,
                listener_pid: None,
                detail: None,
            },
        }))
    }
    async fn process_fingerprint(&self, pid: i64) -> Result<ProcessFingerprint, BrowserRefusal> {
        Ok(ProcessFingerprint {
            pid,
            start_time: Some(1),
            executable: None,
        })
    }
    async fn prepare_endpoint(&self, _: PrepareRequest) -> Result<PrepareOutcome, BrowserRefusal> {
        Err(BrowserRefusal::new(
            BrowserRefusalCode::BrowserRouteUnavailable,
            "readonly capture fixture",
        ))
    }
}
struct State {
    calls: Vec<(String, Value)>,
    selected: bool,
    navigate_on_capture: bool,
    switch_on_capture: bool,
    loader: &'static str,
    fail_capture: bool,
    png: String,
}
struct Fixture {
    engine: Arc<BrowserEngine>,
    state: Arc<Mutex<State>>,
    gone: Arc<Mutex<bool>>,
    target: String,
    tab: String,
    _server: MockCdpServer,
}
fn png(width: u32, height: u32) -> String {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            &vec![100; width as usize * height as usize * 3],
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    BASE64.encode(bytes)
}
async fn fixture() -> Fixture {
    let state = Arc::new(Mutex::new(State {
        calls: vec![],
        selected: true,
        navigate_on_capture: false,
        switch_on_capture: false,
        loader: "L1",
        fail_capture: false,
        png: png(4, 2),
    }));
    let shared = state.clone();
    let server = MockCdpServer::start(Arc::new(move |call| {
        let mut state = shared.lock().unwrap();
        state.calls.push((call.method.clone(), call.params.clone()));
        let value = match call.method.as_str() {
            "Target.getTargets" => json!({"targetInfos":[{"targetId":"T1","type":"page","title":"Capture fixture","url":"https://capture.test/"}]}),
            "Browser.getWindowForTarget" => json!({"windowId":1,"bounds":{"left":0,"top":0,"width":800,"height":600,"windowState":"normal"}}),
            "Browser.getWindowBounds" => json!({"bounds":{"left":0,"top":0,"width":800,"height":600,"windowState":"normal"}}),
            "Target.attachToTarget" => json!({"sessionId":"s"}),
            "Target.setAutoAttach" | "Page.enable" => json!({}),
            "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"F1","loaderId":state.loader,"url":"https://capture.test/"}}}),
            "Runtime.evaluate" => {
                assert_eq!(call.params["expression"], "document.visibilityState === 'visible'");
                json!({"result":{"value":state.selected}})
            }
            "Page.getLayoutMetrics" => json!({"cssVisualViewport":{"pageX":17,"pageY":23,"clientWidth":2,"clientHeight":1}}),
            "Page.captureScreenshot" => {
                if state.navigate_on_capture {state.loader = "L2";}
                if state.switch_on_capture {state.selected = false;}
                if state.fail_capture {return MockReply::err(-32000, "native view capture unavailable");}
                json!({"data":state.png})
            }
            method => return MockReply::method_not_found(method),
        };
        MockReply::ok(value)
    })).await;
    let gone = Arc::new(Mutex::new(false));
    let engine = BrowserEngine::new(Arc::new(Platform {
        endpoint: server.ws_url(),
        gone: gone.clone(),
    }));
    let bound = GetBrowserStateTool::new(engine.clone())
        .invoke(json!({"pid":1,"window_id":7,"session":"capture-mode-tests"}))
        .await;
    let value = bound.structured_content.unwrap();
    assert_eq!(value["status"], "ok", "{value}");
    Fixture {
        engine,
        state,
        gone,
        target: value["target_id"].as_str().unwrap().into(),
        tab: value["tabs"][0]["tab_id"].as_str().unwrap().into(),
        _server: server,
    }
}
async fn capture(
    f: &Fixture,
    mode: BrowserScreenshotMode,
) -> Result<super::BrowserTabScreenshot, BrowserRefusal> {
    f.engine
        .capture_tab_screenshot("capture-mode-tests", &f.target, &f.tab, mode)
        .await
}
fn captures(f: &Fixture) -> Vec<Value> {
    f.state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(method, _)| method == "Page.captureScreenshot")
        .map(|(_, params)| params.clone())
        .collect()
}
fn assert_no_input(f: &Fixture) {
    for (method, _) in &f.state.lock().unwrap().calls {
        assert!(
            !method.starts_with("Input.")
                && !method.starts_with("Emulation.")
                && method != "Target.activateTarget"
                && method != "Page.bringToFront",
            "unexpected {method}"
        );
    }
}
#[tokio::test]
async fn unclipped_modes_submit_once_without_clip_or_unproven_css_geometry() {
    for mode in [
        BrowserScreenshotMode::SurfaceUnclipped,
        BrowserScreenshotMode::NativeView,
    ] {
        let f = fixture().await;
        let result = capture(&f, mode).await.unwrap();
        assert_eq!(result.mode, mode);
        assert_eq!((result.width, result.height), (4, 2));
        assert_eq!(
            (
                result.viewport_css_width,
                result.viewport_css_height,
                result.pixel_to_css_scale_x,
                result.pixel_to_css_scale_y
            ),
            (None, None, None, None)
        );
        assert_eq!(
            captures(&f),
            vec![
                json!({"format":"png","fromSurface":mode == BrowserScreenshotMode::SurfaceUnclipped,"captureBeyondViewport":false})
            ]
        );
        assert!(!f
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(method, _)| method == "Page.getLayoutMetrics"));
        assert_no_input(&f);
    }
}
#[tokio::test]
async fn legacy_viewport_keeps_clip_and_inactive_tab_behavior() {
    let f = fixture().await;
    f.state.lock().unwrap().selected = false;
    let result = capture(&f, BrowserScreenshotMode::Viewport).await.unwrap();
    assert_eq!(
        (
            result.viewport_css_width,
            result.viewport_css_height,
            result.pixel_to_css_scale_x,
            result.pixel_to_css_scale_y
        ),
        (Some(2.), Some(1.), Some(0.5), Some(0.5))
    );
    assert_eq!(
        captures(&f),
        vec![
            json!({"format":"png","fromSurface":true,"captureBeyondViewport":false,"clip":{"x":17.,"y":23.,"width":2.,"height":1.,"scale":1.}})
        ]
    );
    assert_no_input(&f);
}
#[tokio::test]
async fn unselected_or_closed_sources_refuse_before_capture() {
    for gone in [false, true] {
        let f = fixture().await;
        if gone {
            *f.gone.lock().unwrap() = true;
        } else {
            f.state.lock().unwrap().selected = false;
        }
        let refusal = capture(&f, BrowserScreenshotMode::NativeView)
            .await
            .err()
            .unwrap();
        assert_eq!(refusal.code, BrowserRefusalCode::BrowserWrongTargetRefused);
        assert!(captures(&f).is_empty());
        assert_no_input(&f);
    }
}
#[tokio::test]
async fn document_or_selection_changes_during_capture_refuse_without_fallback() {
    for navigated in [false, true] {
        let f = fixture().await;
        if navigated {
            f.state.lock().unwrap().navigate_on_capture = true;
        } else {
            f.state.lock().unwrap().switch_on_capture = true;
        }
        let refusal = capture(&f, BrowserScreenshotMode::SurfaceUnclipped)
            .await
            .err()
            .unwrap();
        assert_eq!(
            refusal.code,
            if navigated {
                BrowserRefusalCode::BrowserRefStale
            } else {
                BrowserRefusalCode::BrowserWrongTargetRefused
            }
        );
        assert_eq!(captures(&f).len(), 1);
        assert_no_input(&f);
    }
}
#[tokio::test]
async fn cancelled_wait_for_typed_operation_gate_cannot_dispatch_capture() {
    let f = fixture().await;
    let held = f
        .engine
        .lock_mutation("capture-mode-tests", &f.target, &f.tab)
        .await
        .unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(20),
        capture(&f, BrowserScreenshotMode::NativeView)
    )
    .await
    .is_err());
    assert!(captures(&f).is_empty());
    drop(held);
    capture(&f, BrowserScreenshotMode::NativeView)
        .await
        .unwrap();
    assert_eq!(captures(&f).len(), 1);
    assert_no_input(&f);
}

#[tokio::test]
async fn untrusted_png_dimensions_are_bounded_before_any_bitmap_use() {
    for (width, height) in [(0u32, 2u32), (16385, 1), (4097, 4097)] {
        let f = fixture().await;
        let mut data = BASE64.decode(png(1, 1)).unwrap();
        data[16..20].copy_from_slice(&width.to_be_bytes());
        data[20..24].copy_from_slice(&height.to_be_bytes());
        f.state.lock().unwrap().png = BASE64.encode(data);
        let refusal = capture(&f, BrowserScreenshotMode::SurfaceUnclipped)
            .await
            .err()
            .unwrap();
        assert_eq!(refusal.code, BrowserRefusalCode::BrowserRouteUnavailable);
        assert_eq!(captures(&f).len(), 1);
        assert_no_input(&f);
    }
}

#[tokio::test]
async fn native_capture_error_never_substitutes_surface_or_replays() {
    let f = fixture().await;
    f.state.lock().unwrap().fail_capture = true;
    let refusal = capture(&f, BrowserScreenshotMode::NativeView)
        .await
        .err()
        .unwrap();
    assert_eq!(refusal.code, BrowserRefusalCode::BrowserRouteUnavailable);
    assert_eq!(
        captures(&f),
        vec![json!({"format":"png","fromSurface":false,"captureBeyondViewport":false})]
    );
    assert_no_input(&f);
}
