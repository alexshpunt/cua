//! Public readonly inspection contracts against a deterministic CDP endpoint.
use crate::browser::engine::BrowserEngine;
use crate::browser::mock_cdp::{MockCdpServer, MockReply};
use crate::browser::platform::{BrowserPlatform, PrepareOutcome, PrepareRequest};
use crate::browser::refusal::{BrowserRefusal, BrowserRefusalCode};
use crate::browser::tools::{BrowserClickTool, GetBrowserStateTool};
use crate::browser::types::*;
use crate::tool::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    calls: Vec<(String, Value)>,
    navigated: bool,
    detached: bool,
    selected: bool,
    visible: bool,
    duplicate_title: bool,
    visibility_reply: Option<Value>,
    fail_visibility: bool,
    switch_during_collection: bool,
    oopif: bool,
    node_detached: bool,
    bad_metadata: bool,
    unmatched_css_node: bool,
    single_frame: bool,
    missing_layout_identity: bool,
    long_live_value: bool,
}
struct Platform {
    endpoint: String,
    state: Arc<Mutex<State>>,
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
        Ok(NativeWindowInfo {
            pid,
            window_id,
            title: if self.state.lock().unwrap().selected {
                "Fixture"
            } else {
                "Other tab"
            }
            .into(),
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
            "readonly fixture",
        ))
    }
}
struct Fixture {
    engine: Arc<BrowserEngine>,
    state: Arc<Mutex<State>>,
    _server: MockCdpServer,
}
fn element(id: i64, backend: i64, tag: &str, attributes: Value, children: Vec<Value>) -> Value {
    json!({"nodeType":1,"nodeId":id,"backendNodeId":backend,"nodeName":tag,"attributes":attributes,"childNodeCount":children.len(),"children":children})
}
fn document(detached: bool) -> Value {
    let mut iframe = element(13, 13, "IFRAME", json!([]), vec![]);
    if !detached {
        iframe["contentDocument"] = json!({"nodeType":9,"nodeId":200,"backendNodeId":200,"frameId":"F_IFRAME","childNodeCount":1,
        "children":[element(30,10,"BUTTON",json!(["id","inner-btn","aria-label","Inner"]),vec![])]});
    }
    let mut shadow = element(14, 14, "DIV", json!([]), vec![]);
    shadow["shadowRoots"] = json!([{"nodeType":11,"nodeId":150,"backendNodeId":150,"shadowRootType":"open","childNodeCount":1,
        "children":[element(15,15,"BUTTON",json!(["data-testid","shadow","aria-label","Shadow"]),vec![])]}]);
    json!({"root":{"nodeType":9,"nodeId":100,"backendNodeId":100,"frameId":"F_MAIN","documentURL":"https://fixture.test/","childNodeCount":1,
        "children":[element(1,1,"HTML",json!([]),vec![
            element(10,10,"BUTTON",json!(["id","main-btn","data-testid","save","aria-label","Save"]),vec![json!({"nodeType":3,"nodeId":110,"backendNodeId":110,"nodeValue":"Save draft"})]),
            element(11,11,"BUTTON",json!(["hidden","","aria-label","Save"]),vec![]),
            element(12,12,"INPUT",json!(["value","markup-only","type","checkbox"]),vec![]),iframe,shadow]) ]}})
}
async fn bound() -> (Fixture, String, String) {
    let state = Arc::new(Mutex::new(State {
        selected: true,
        visible: true,
        ..Default::default()
    }));
    let shared = state.clone();
    let server=MockCdpServer::start(Arc::new(move|call| {
        let mut state=shared.lock().unwrap(); state.calls.push((call.method.clone(),call.params.clone()));
        let result=match call.method.as_str() {
            "Target.getTargets"=> {
                let mut targets=vec![json!({"targetId":"T1","type":"page","title":"Fixture","url":"https://fixture.test/"})];
                if !state.selected || state.duplicate_title { targets.push(json!({"targetId":"T2","type":"page","title":if state.duplicate_title {"Fixture"} else {"Other tab"},"url":"https://other.test/"})); }
                json!({"targetInfos":targets})
            },
            "Browser.getWindowForTarget"=>json!({"windowId":1,"bounds":{"left":0,"top":0,"width":800,"height":600,"windowState":"normal"}}),
            "Browser.getWindowBounds"=>json!({"bounds":{"left":0,"top":0,"width":800,"height":600,"windowState":"normal"}}),
            "Target.attachToTarget"=>json!({"sessionId":"s"}),
            "Target.setAutoAttach"=> {
                if state.oopif {
                    return MockReply::ok(json!({})).with_events(vec![crate::browser::mock_cdp::MockEvent {
                        method:"Target.attachedToTarget".into(),session_id:call.session_id.clone(),
                        params:json!({"sessionId":"child","targetInfo":{"type":"iframe","targetId":"C1","url":"https://cross.test/"}})
                    }]);
                }
                json!({})
            },
            "Page.enable"=>json!({}),
            "Page.createIsolatedWorld"=> {
                assert_eq!(call.params["frameId"],"F_MAIN");
                assert_eq!(call.params["grantUniveralAccess"],false);
                json!({"executionContextId":77})
            },
            "Page.getFrameTree"=> {
                if call.session_id.as_deref()==Some("child") {return MockReply::ok(json!({"frameTree":{"frame":{"id":"O_FRAME","loaderId":"O_LOADER","url":"https://cross.test/"}}}));}
                let mut tree=json!({"frameTree":{"frame":{"id":"F_MAIN","loaderId":if state.navigated {"NEW"} else {"L_MAIN"},"url":"https://fixture.test/"}}});
                if !state.detached && !state.single_frame {tree["frameTree"]["childFrames"]=json!([{"frame":{"id":"F_IFRAME","loaderId":"L_CHILD","url":"https://fixture.test/child"}}]);} tree
            },
            "DOM.getDocument"=> {
                if state.switch_during_collection {state.selected=false;}
                if state.single_frame {
                    return MockReply::ok(json!({"root":{"nodeType":9,"nodeId":100,"backendNodeId":100,"frameId":"F_MAIN","childNodeCount":1,
                        "children":[element(12,12,"INPUT",json!(["data-testid","live-value"]),vec![])]}}));
                }
                if call.session_id.as_deref()==Some("child") {
                    json!({"root":{"nodeType":9,"nodeId":300,"backendNodeId":300,"frameId":"O_FRAME","childNodeCount":1,
                        "children":[element(30,10,"BUTTON",json!(["id","cross-btn","aria-label","Cross"]),vec![])]}})
                } else {document(state.detached)}
            },
            "DOM.describeNode"=>if state.detached || state.node_detached {return MockReply::err(-32000,"detached node")} else {json!({"node":{"backendNodeId":10,"nodeId":30}})},
            "DOM.querySelectorAll"=> {
                assert_eq!(call.params["selector"],"button[data-testid='save']:not([disabled])");
                json!({"nodeIds":if call.params["nodeId"]==100 {if state.unmatched_css_node {vec![999]} else {vec![10]}} else {vec![]}})
            },
            "Runtime.evaluate"=> {
                assert_eq!(call.params["expression"],"document.visibilityState === 'visible'");
                assert_eq!(call.params["contextId"],77);
                if state.fail_visibility {return MockReply::err(-32000,"visibility unavailable");}
                state.visibility_reply.clone().unwrap_or_else(||json!({"result":{"value":state.visible}}))
            },
            "Accessibility.getFullAXTree"=>if call.params["frameId"]=="O_FRAME" {json!({"nodes":[{"backendDOMNodeId":10,"role":{"value":"button"},"name":{"value":"Cross"}}]})} else if call.params["frameId"]=="F_IFRAME" {json!({"nodes":[{"backendDOMNodeId":10,"role":{"value":"button"},"name":{"value":"Inner"}}]})}
                else {json!({"nodes":[{"backendDOMNodeId":10,"role":{"value":"button"},"name":{"value":"Save"}},
                    {"backendDOMNodeId":11,"role":{"value":"button"},"name":{"value":"Save"}},
                    {"backendDOMNodeId":12,"role":{"value":"checkbox"},"value":{"value":"live-value"},"properties":[{"name":"checked","value":{"value":true}},{"name":"disabled","value":{"value":false}}]}]})},
            "DOMSnapshot.captureSnapshot"=>if state.bad_metadata {json!({})} else if state.single_frame {
                json!({"strings":["F_MAIN","block","visible","1",if state.long_live_value {"é".repeat(1025)} else {"live-value".to_owned()}],
                    "documents":[{"frameId":0,"nodes":{"backendNodeId":if state.missing_layout_identity {vec![999]} else {vec![12]},
                        "inputValue":{"index":[0],"value":[4]}},
                        "layout":{"nodeIndex":[0],"bounds":[[10,20,80,30]],"styles":[[1,2,3]]}}]})
            } else if call.session_id.as_deref()==Some("child") {
                json!({"strings":["O_FRAME","block","visible","1"],"documents":[{"frameId":0,"nodes":{"backendNodeId":[10]},
                    "layout":{"nodeIndex":[0],"bounds":[[5,6,40,20]],"styles":[[1,2,3]]}}]})
            } else {json!({"strings":["F_MAIN","F_IFRAME","block","visible","1"],"documents":[
                {"frameId":0,"nodes":{"backendNodeId":[1,10,11,12,13,14,15]},"layout":{"nodeIndex":[0,1,3,4,5,6],"bounds":[[0,0,800,600],[10,20,80,30],[5,60,20,20],[0,100,300,200],[0,400,100,50],[0,400,100,50]],"styles":[[2,3,4],[2,3,4],[2,3,4],[2,3,4],[2,3,4],[2,3,4]]}},
                {"frameId":1,"nodes":{"backendNodeId":[10]},"layout":{"nodeIndex":[0],"bounds":[[2,3,40,20]],"styles":[[2,3,4]]}}]})},
            method=>return MockReply::method_not_found(method)
        }; MockReply::ok(result)
    })).await;
    let engine = BrowserEngine::new(Arc::new(Platform {
        endpoint: server.ws_url(),
        state: state.clone(),
    }));
    let value = structured(
        &GetBrowserStateTool::new(engine.clone())
            .invoke(json!({"pid":1,"window_id":7,"session":"dom-inspection-tests"}))
            .await,
    );
    assert_eq!(value["status"], "ok", "{value}");
    let target = value["target_id"].as_str().unwrap().to_owned();
    let tab = value["tabs"][0]["tab_id"].as_str().unwrap().to_owned();
    (
        Fixture {
            engine,
            state,
            _server: server,
        },
        target,
        tab,
    )
}
fn structured(result: &crate::protocol::ToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("structured browser result")
}
fn args(target: &str, tab: &str) -> Value {
    json!({"target_id":target,"tab_id":tab,"session":"dom-inspection-tests","snapshot_format":"dom_inspect_v1"})
}

#[tokio::test]
async fn selected_hidden_document_remains_readonly_inspectable() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().visible = false;
    let result = structured(
        &GetBrowserStateTool::new(f.engine.clone())
            .invoke(args(&target, &tab))
            .await,
    );
    assert_eq!(result["status"], "ok", "{result}");
    assert!(!result["elements"].as_array().unwrap().is_empty());
    assert!(f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .all(|(method, _)| !method.starts_with("Input.")
            && !matches!(
                method.as_str(),
                "Target.activateTarget" | "Page.bringToFront" | "Runtime.callFunctionOn"
            )));
}

#[tokio::test]
async fn visible_document_cannot_replace_fresh_selected_target_evidence() {
    for duplicate in [false, true] {
        let (f, target, tab) = bound().await;
        if duplicate {
            f.state.lock().unwrap().duplicate_title = true;
        } else {
            f.state.lock().unwrap().selected = false;
        }
        let result = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(args(&target, &tab))
                .await,
        );
        assert_eq!(
            result["refusal"]["code"], "browser_wrong_target_refused",
            "{result}"
        );
        assert!(f
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _)| method != "DOM.getDocument"));
    }
}

#[tokio::test]
async fn invalid_isolated_visibility_is_unavailable_not_a_selected_tab_change() {
    for reply in [
        json!({}),
        json!({"result":{"value":null}}),
        json!({"result":{"value":"visible"}}),
        json!({"result":{"value":true},"exceptionDetails":{"text":"failed"}}),
    ] {
        let (f, target, tab) = bound().await;
        f.state.lock().unwrap().visibility_reply = Some(reply);
        let result = structured(
            &GetBrowserStateTool::new(f.engine.clone())
                .invoke(args(&target, &tab))
                .await,
        );
        assert_eq!(
            result["refusal"]["code"], "browser_route_unavailable",
            "{result}"
        );
        assert!(f
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _)| method != "DOM.getDocument"));
    }
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().fail_visibility = true;
    let result = structured(
        &GetBrowserStateTool::new(f.engine.clone())
            .invoke(args(&target, &tab))
            .await,
    );
    assert_eq!(
        result["refusal"]["code"], "browser_route_unavailable",
        "{result}"
    );
}

#[tokio::test]
async fn missing_layout_identity_cannot_mint_a_complete_readonly_record() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().single_frame = true;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let baseline = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(baseline["snapshot"]["complete"], true, "{baseline}");
    assert_eq!(baseline["elements"].as_array().unwrap().len(), 1);
    f.state.lock().unwrap().missing_layout_identity = true;
    let result = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(result["snapshot"]["complete"], false, "{result}");
    assert!(result["snapshot"]["omitted"]["metadata"].as_u64().unwrap() > 0);
    assert_eq!(
        result["elements"],
        json!([]),
        "Detached identities must not mint refs."
    );
}

#[tokio::test]
async fn long_live_values_report_bounded_text_and_partial_coverage() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().single_frame = true;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let baseline = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(baseline["snapshot"]["complete"], true, "{baseline}");
    assert_eq!(baseline["elements"][0]["value"], "live-value");
    f.state.lock().unwrap().long_live_value = true;
    let result = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(
        result["elements"][0]["value"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1024
    );
    assert_eq!(result["elements"][0]["text_truncated"], true);
    assert_eq!(result["snapshot"]["complete"], false);
    assert!(result["snapshot"]["omitted"]["text"].as_u64().unwrap() > 0);
}
#[tokio::test]
async fn missing_metadata_and_uncollected_css_matches_cannot_prove_absence() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    f.state.lock().unwrap().bad_metadata = true;
    let mut request = args(&target, &tab);
    request["locator"] = json!({"name":"absent"});
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["snapshot"]["complete"], false);
    assert!(result["snapshot"]["omitted"]["metadata"].as_u64().unwrap() > 0);
    f.state.lock().unwrap().bad_metadata = false;
    f.state.lock().unwrap().unmatched_css_node = true;
    let mut request = args(&target, &tab);
    request["locator"] = json!({"css":"button[data-testid='save']:not([disabled])"});
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["elements"], json!([]));
    assert_eq!(result["snapshot"]["complete"], false);
    assert!(result["snapshot"]["omitted"]["nodes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn contained_oopif_refs_keep_child_source_and_refuse_detached_children() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().oopif = true;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let full = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(full["status"], "ok", "{full}");
    let elements = full["elements"].as_array().unwrap();
    let cross = elements
        .iter()
        .find(|e| e["attributes"]["id"] == "cross-btn")
        .unwrap();
    assert_eq!(cross["frame"]["kind"], "oopif");
    assert_eq!(
        cross["bounds_css"],
        json!({"x":5.0,"y":6.0,"width":40.0,"height":20.0})
    );
    let mut scoped = args(&target, &tab);
    scoped["scope_ref"] = cross["ref"].clone();
    f.state.lock().unwrap().oopif = false;
    let refusal = structured(&tool.invoke(scoped).await);
    assert_eq!(refusal["refusal"]["code"], "browser_ref_stale", "{refusal}");
}

#[tokio::test]
async fn selected_tab_changes_and_detached_nodes_do_not_mint_new_authority() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let full = structured(&tool.invoke(args(&target, &tab)).await);
    let old = full["elements"][0]["ref"].as_str().unwrap();
    let mut scoped = args(&target, &tab);
    scoped["scope_ref"] = json!(old);
    f.state.lock().unwrap().node_detached = true;
    assert_eq!(
        structured(&tool.invoke(scoped).await)["refusal"]["code"],
        "browser_ref_stale"
    );
    assert!(f
        .engine
        .store
        .resolve_ref("dom-inspection-tests", &target, &tab, old)
        .is_err());
    f.state.lock().unwrap().node_detached = false;
    f.state.lock().unwrap().switch_during_collection = true;
    assert_eq!(
        structured(&tool.invoke(args(&target, &tab)).await)["refusal"]["code"],
        "browser_wrong_target_refused"
    );
}

#[tokio::test]
async fn bounded_native_reply_refuses_oversize_without_poisoning_the_connection() {
    let server = MockCdpServer::start(Arc::new(|_| {
        MockReply::ok(json!({"payload":"x".repeat(256)}))
    }))
    .await;
    let conn = crate::browser::cdp_ws::CdpConnection::connect(&server.ws_url())
        .await
        .unwrap();
    let error = conn
        .call_bounded(None, "DOM.getDocument", json!({}), 64)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("bounded CDP response exceeded its byte budget"));
    assert!(conn.call(None, "DOM.getDocument", json!({})).await.is_ok());
}
#[tokio::test]
async fn dom_inspection_metadata_and_readonly_refs_retire_without_mutation() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let first = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(first["status"], "ok", "{first}");
    let elements = first["elements"].as_array().unwrap();
    assert!(elements.iter().any(|e| e["tag"] == "html"));
    assert!(elements
        .iter()
        .all(|e| e["coordinate_space"] == "document_css_pixels"));
    let input = elements.iter().find(|e| e["tag"] == "input").unwrap();
    assert_eq!(input["value"], "live-value");
    assert_eq!(input["states"]["checked"], true);
    assert_eq!(input["attributes"]["value"], "markup-only");
    let reference = elements
        .iter()
        .find(|e| e["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .as_str()
        .unwrap();
    let click=structured(&BrowserClickTool::new(f.engine.clone()).invoke(json!({"target_id":target,"tab_id":tab,"session":"dom-inspection-tests","ref":reference,"input_route":"dom_event"})).await);
    assert_eq!(
        click["refusal"]["code"], "browser_action_unavailable",
        "{click}"
    );
    assert_eq!(
        structured(&tool.invoke(args(&target, &tab)).await)["status"],
        "ok"
    );
    assert!(f
        .engine
        .store
        .resolve_ref("dom-inspection-tests", &target, &tab, reference)
        .is_err());
    assert!(f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .all(|(method, _)| !method.starts_with("Input.")
            && !matches!(
                method.as_str(),
                "DOM.focus"
                    | "DOM.scrollIntoViewIfNeeded"
                    | "Runtime.callFunctionOn"
                    | "Target.activateTarget"
                    | "Page.bringToFront"
            )));
}
#[tokio::test]
async fn exact_locators_keep_duplicates_and_css_uses_chromium_not_local_matching() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let mut request = args(&target, &tab);
    request["locator"] = json!({"role":"button","name":"Save"});
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["elements"].as_array().unwrap().len(), 2);
    let mut request = args(&target, &tab);
    request["locator"] = json!({"css":"button[data-testid='save']:not([disabled])"});
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["elements"].as_array().unwrap().len(), 1);
    assert_eq!(result["elements"][0]["attributes"]["id"], "main-btn");
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m == "DOM.querySelectorAll")
            .count(),
        3
    );
}
#[tokio::test]
async fn invalid_arguments_refuse_before_cdp_and_partial_absence_is_explicit() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let count = f.state.lock().unwrap().calls.len();
    for (key, value) in [
        ("max_nodes", json!(0)),
        ("max_depth", json!(33)),
        ("include_hidden", json!("true")),
        ("locator", json!({"xpath":"//button"})),
    ] {
        let mut request = args(&target, &tab);
        request[key] = value;
        assert!(tool.invoke(request).await.is_error.unwrap_or(false));
    }
    assert_eq!(count, f.state.lock().unwrap().calls.len());
    let mut request = args(&target, &tab);
    request["max_depth"] = json!(1);
    request["locator"] = json!({"name":"not-present"});
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["snapshot"]["collection_complete"], false);
    assert_eq!(result["snapshot"]["complete"], false);
    assert_eq!(result["elements"], json!([]));
}
#[tokio::test]
async fn frame_qualified_scope_handles_colliding_backend_ids_and_detachment() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let full = structured(&tool.invoke(args(&target, &tab)).await);
    let elements = full["elements"].as_array().unwrap();
    let child = elements
        .iter()
        .find(|e| e["attributes"]["id"] == "inner-btn")
        .unwrap();
    let main = elements
        .iter()
        .find(|e| e["attributes"]["id"] == "main-btn")
        .unwrap();
    assert_ne!(child["frame"]["ref"], main["frame"]["ref"]);
    let mut request = args(&target, &tab);
    request["scope_ref"] = child["ref"].clone();
    let scoped = structured(&tool.invoke(request.clone()).await);
    assert_eq!(scoped["status"], "ok", "{scoped}");
    assert_eq!(scoped["elements"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["elements"][0]["attributes"]["id"], "inner-btn");
    request["scope_ref"] = scoped["elements"][0]["ref"].clone();
    f.state.lock().unwrap().detached = true;
    let refused = structured(&tool.invoke(request).await);
    assert_eq!(refused["refusal"]["code"], "browser_ref_stale", "{refused}");
}
#[tokio::test]
async fn navigated_scope_and_inactive_tab_refuse_without_activation() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let full = structured(&tool.invoke(args(&target, &tab)).await);
    let mut request = args(&target, &tab);
    request["scope_ref"] = full["elements"][0]["ref"].clone();
    f.state.lock().unwrap().navigated = true;
    assert_eq!(
        structured(&tool.invoke(request).await)["refusal"]["code"],
        "browser_ref_stale"
    );
    f.state.lock().unwrap().selected = false;
    let result = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(result["refusal"]["code"], "browser_wrong_target_refused");
}
