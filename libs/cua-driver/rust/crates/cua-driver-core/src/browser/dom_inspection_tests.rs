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
    dom_document: Option<Value>,
    dom_layout: Option<Value>,
    missing_layout_identity: bool,
    long_live_value: bool,
    distinct_attachment_ids: bool,
    geometry_missing_unique: bool,
    geometry_context_fault: String,
    geometry_exception: bool,
    actionability_reply: Option<Value>,
    actionability_eval_error: bool,
    actionability_sample_changes: bool,
    actionability_reads: usize,
    sampled_backend: i64,
    hit_backend: Option<i64>,
    hit_frame: Option<String>,
    hit_relation: Option<Value>,
    hit_exception: bool,
    hit_changes: bool,
    hit_navigates: bool,
    hit_description_fault: String,
    hit_reads: usize,
    hit_resolving: bool,
    fractional_geometry: bool,
    geometry_detaches: bool,
    geometry_navigates: bool,
    geometry_cleanup_fails: bool,
    geometry_resolved: Option<Arc<tokio::sync::Notify>>,
    geometry_released: Option<Arc<tokio::sync::Notify>>,
    dispatch_fail_step: String,
    dispatch_navigate_step: String,
    dispatch_signal: Option<Arc<tokio::sync::Notify>>,
    dispatch_preflight_refused: bool,
    dispatch_negative_step: String,
    dispatch_signal_step: String,
    dispatch_stall_step: String,
    dispatch_missing_unique: bool,
    dispatch_cleanup_fails: bool,
    dispatch_released: Option<Arc<tokio::sync::Notify>>,
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
#[path = "dom_action_dispatch_tests.rs"]
mod action_dispatch_tests;
#[path = "dom_action_hit_tests.rs"]
mod action_hit_tests;
#[path = "dom_action_prepare_tests.rs"]
mod action_prepare_tests;
#[path = "dom_action_tool_tests.rs"]
mod action_tool_tests;
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
// A realistic authored form plus native UA roots. UA descendants deliberately
// contain forbidden extra frame/query roots and more than the author visit budget.
fn author_form_fixture() -> (Value, Value) {
    let root = |id: i64, children: Vec<Value>| {
        json!({"nodeType":11,"nodeId":id,
        "backendNodeId":id,"shadowRootType":"user-agent","childNodeCount":children.len(),"children":children})
    };
    let mut private_frame = element(901, 901, "IFRAME", json!([]), vec![]);
    private_frame["contentDocument"] = json!({"nodeType":9,"nodeId":902,"backendNodeId":902,
        "frameId":"PRIVATE_UA_FRAME","childNodeCount":0,"children":[]});
    let mut private_host = element(903, 903, "DIV", json!([]), vec![]);
    private_host["shadowRoots"] = json!([{"nodeType":11,"nodeId":904,"backendNodeId":904,
        "shadowRootType":"open","childNodeCount":0,"children":[]}]);
    let mut internals = vec![private_frame, private_host];
    internals.extend((10_000..13_000).map(|id| element(id, id, "DIV", json!([]), vec![])));
    let mut input = element(
        12,
        12,
        "INPUT",
        json!(["data-testid", "live-value"]),
        vec![],
    );
    input["shadowRoots"] = json!([root(101, internals)]);
    let options = (14..=16)
        .map(|id| {
            let mut option = element(id, id, "OPTION", json!(["value", id.to_string()]), vec![]);
            option["shadowRoots"] = json!([root(id + 100, vec![])]);
            option
        })
        .collect();
    let mut select = element(13, 13, "SELECT", json!([]), options);
    select["shadowRoots"] = json!([root(113, vec![])]);
    let button = element(
        10,
        10,
        "BUTTON",
        json!([
            "id",
            "main-btn",
            "data-testid",
            "save",
            "aria-label",
            "Save"
        ]),
        vec![],
    );
    let mut shadow = element(20, 20, "DIV", json!([]), vec![]);
    shadow["shadowRoots"] = json!([{"nodeType":11,"nodeId":150,"backendNodeId":150,
        "shadowRootType":"open","childNodeCount":1,"children":[element(21,21,"BUTTON",json!([]),vec![])]}]);
    let document = json!({"root":{"nodeType":9,"nodeId":100,"backendNodeId":100,"frameId":"F_MAIN",
        "documentURL":"https://fixture.test/","childNodeCount":4,"children":[input,select,button,shadow]}});
    let backends = vec![12, 13, 14, 15, 16, 10, 20, 21];
    let indices = (0..backends.len()).collect::<Vec<_>>();
    let layout = json!({"strings":["F_MAIN","block","visible","1"],"documents":[{
        "frameId":0,"nodes":{"backendNodeId":backends},
        "layout":{"nodeIndex":indices,"styles":vec![vec![1,2,3];8]}
    }]});
    (document, layout)
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
            "Target.attachToTarget"=>json!({"sessionId":if state.distinct_attachment_ids {
                format!("s-{}", state.calls.iter().filter(|(method,_)|method=="Target.attachToTarget").count())
            } else {"s".into()}}),
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
            "Runtime.enable"=>json!({}),
            "DOM.resolveNode"=> {
                assert_eq!(call.params["executionContextId"],77);
                assert!(call.params["objectGroup"].as_str().is_some());
                if let Some(signal) = &state.geometry_resolved { signal.notify_one(); }
                let object = if state.hit_resolving { state.hit_resolving=false; "hit-node" } else {
                    state.sampled_backend=call.params["backendNodeId"].as_i64().unwrap(); "geometry-node"
                };
                json!({"object":{"type":"object","subtype":"node","objectId":object}})
            },
            "DOM.getNodeForLocation"=> {
                assert_eq!(call.params["x"],50); assert_eq!(call.params["y"],155);
                assert_eq!(call.params["includeUserAgentShadowDOM"],false);
                assert_eq!(call.params["ignorePointerEventsNone"],false);
                state.hit_reads+=1; state.hit_resolving=true;
                if state.hit_navigates {state.navigated=true;}
                let backend=state.hit_backend.unwrap_or(state.sampled_backend) + if state.hit_changes && state.hit_reads%2==0 {1} else {0};
                json!({"backendNodeId":backend,"frameId":state.hit_frame.as_deref().unwrap_or("F_MAIN")})
            },
            "Runtime.callFunctionOn"=> {
                assert_eq!(call.params["uniqueContextId"],"unique-main-77");
                assert!(call.params.get("executionContextId").is_none());
                assert!(call.params.get("objectId").is_none());
                let function = call.params["functionDeclaration"].as_str().unwrap();
                if function == crate::browser::dom_action_operation::PREFLIGHT_FUNCTION
                    || function == crate::browser::dom_action_operation::STEP_FUNCTION {
                    assert_eq!(call.params["arguments"][0],json!({"objectId":"geometry-node"}));
                    assert_eq!(call.params["throwOnSideEffect"], function == crate::browser::dom_action_operation::PREFLIGHT_FUNCTION);
                    assert_eq!(call.params["returnByValue"],true);
                    assert_eq!(call.params["awaitPromise"],false);
                    assert_eq!(call.params["userGesture"],false);
                    if function == crate::browser::dom_action_operation::PREFLIGHT_FUNCTION {
                        return MockReply::ok(json!({"result":{"value":{"ok":!state.dispatch_preflight_refused}}}));
                    }
                    let step = call.params["arguments"][1]["value"]["name"].as_str().unwrap();
                    if state.dispatch_signal_step.is_empty() || state.dispatch_signal_step==step {
                        if let Some(signal)=&state.dispatch_signal {signal.notify_one();}
                    }
                    if state.dispatch_stall_step==step {return MockReply::ok(json!({})).without_response();}
                    if state.dispatch_navigate_step == step {state.navigated=true;}
                    if state.dispatch_negative_step == step {
                        return MockReply::ok(json!({"result":{"value":{"ok":false,"reason":"dom_action_event_cancelled"}}}));
                    }
                    if state.dispatch_fail_step == step {
                        return MockReply::ok(json!({"exceptionDetails":{"text":"unknown effect"}}));
                    }
                    return MockReply::ok(json!({"result":{"value":{"ok":true,"observed":{"focused":true,"foreign":"discard"}}}}));
                }
                if function == crate::browser::dom_action_hit::HIT_FUNCTION {
                    assert_eq!(call.params["arguments"],json!([{"objectId":"geometry-node"},{"objectId":"hit-node"}]));
                    assert_eq!(call.params["throwOnSideEffect"],true); assert_eq!(call.params["returnByValue"],true);
                    assert_eq!(call.params["awaitPromise"],false);assert_eq!(call.params["userGesture"],false);
                    if state.hit_exception {return MockReply::ok(json!({"exceptionDetails":{"exception":{"className":"EvalError","description":"PRIVATE PROSE"}}}));}
                    return MockReply::ok(json!({"result":{"type":"object","value":{
                        "connected":state.hit_description_fault!="detached_hit", "same_document":state.hit_description_fault!="foreign_document",
                        "relation":state.hit_relation.clone().unwrap_or(json!(true))
                    }}}));
                }
                assert_eq!(call.params["arguments"],json!([{"objectId":"geometry-node"}]));
                assert_eq!(call.params["throwOnSideEffect"],true);
                assert_eq!(call.params["returnByValue"],true);
                assert_eq!(call.params["awaitPromise"],false);
                assert_eq!(call.params["userGesture"],false);
                if state.geometry_navigates {state.navigated=true;}
                if state.geometry_exception {json!({"exceptionDetails":{"text":"side effect check refused"}})} else if call.params["functionDeclaration"] == crate::browser::dom_action_sample::READ_FUNCTION {
                    if state.actionability_eval_error {return MockReply::ok(json!({"exceptionDetails":{"exception":{"className":"EvalError","description":"PRIVATE SCRIPT/URL"}}}));}
                    state.actionability_reads+=1;
                    let mut reply=state.actionability_reply.clone().unwrap_or_else(|| json!({"result":{"type":"object","value":{
                        "connected":!state.geometry_detaches,"same_document":true,
                        "bounds":[10.5,20.25,80.0,30.0], "visible":true,"enabled":true,"editable":false,"receives_pointer":null,
                        "hit_test":{"x":50,"y":155,"scroll_x":0.0,"scroll_y":120.0,"width":800.0,"height":600.0}
                    }}}));
                    if state.actionability_sample_changes && state.actionability_reads>=2 {reply["result"]["value"]["bounds"][0]=json!(11.5);}
                    reply
                } else {
                    assert_eq!(call.params["functionDeclaration"], crate::browser::dom_cssom_sample::READ_FUNCTION);
                    json!({"result":{"type":"object","value":{
                        "connected":!state.geometry_detaches,"same_document":true,
                        "bounds":{"x":10.5,"y":if state.fractional_geometry {182.89584350585938} else {20.25},
                            "width":if state.fractional_geometry {241.33334350585938} else {80.0},"height":30.0},
                        "scroll":{"x":0.0,"y":120.0},
                        "visual_viewport":{"offset_x":0.0,"offset_y":0.0,
                            "page_x":0.0,"page_y":120.0,"width":800.0,"height":600.0,"scale":1.0}
                    }}})
                }
            },
            "Runtime.releaseObjectGroup"=> {
                if call.params["objectGroup"].as_str().is_some_and(|group|group.starts_with("cua-dom-action-")) {
                    if let Some(signal)=&state.dispatch_released {signal.notify_one();}
                    if state.dispatch_cleanup_fails {return MockReply::err(-32000,"owned cleanup failed");}
                }
                if let Some(signal) = &state.geometry_released { signal.notify_one(); }
                if state.geometry_cleanup_fails {return MockReply::err(-32000,"release failed");}
                json!({})
            },
            "Page.createIsolatedWorld"=> {
                assert_eq!(call.params["frameId"],"F_MAIN");
                assert_eq!(call.params["grantUniveralAccess"],false);
                if matches!(call.params["worldName"].as_str(), Some("cua-driver-dom-geometry" | "cua-driver-dom-actionability" | "cua-driver-dom-actions")) {
                    let world_name = call.params["worldName"].as_str().unwrap();
                    let fault = state.geometry_context_fault.as_str();
                    let event = crate::browser::mock_cdp::MockEvent {
                        method:"Runtime.executionContextCreated".into(),
                        session_id:if fault == "session" {Some("foreign-session".into())} else {call.session_id.clone()},
                        params:json!({"context":{"id":if fault == "numeric_id" {78} else {77},
                            "name":if fault == "name" {"page-world"} else {world_name},
                            "uniqueId":if state.geometry_missing_unique || (world_name=="cua-driver-dom-actions" && state.dispatch_missing_unique) {Value::Null} else {json!("unique-main-77")},
                            "auxData":{"frameId":if fault == "frame" {"FOREIGN"} else {"F_MAIN"},"isDefault":fault == "default_world"}}})
                    };
                    let mut events = vec![event];
                    if fault == "reused_id" {
                        events.push(crate::browser::mock_cdp::MockEvent {
                            method:"Runtime.executionContextCreated".into(),session_id:call.session_id.clone(),
                            params:json!({"context":{"id":77,"name":world_name,
                                "uniqueId":"reused-unique-77","auxData":{"frameId":"F_MAIN","isDefault":false}}})
                        });
                    }
                    return MockReply::ok(json!({"executionContextId":77})).with_events(events);
                }
                json!({"executionContextId":77})
            },
            "Page.getFrameTree"=> {
                if call.session_id.as_deref()==Some("child") {return MockReply::ok(json!({"frameTree":{"frame":{"id":"O_FRAME","loaderId":"O_LOADER","url":"https://cross.test/"}}}));}
                let mut tree=json!({"frameTree":{"frame":{"id":"F_MAIN","loaderId":if state.navigated {"NEW"} else {"L_MAIN"},"url":"https://fixture.test/"}}});
                if !state.detached && !state.single_frame {tree["frameTree"]["childFrames"]=json!([{"frame":{"id":"F_IFRAME","loaderId":"L_CHILD","url":"https://fixture.test/child"}}]);} tree
            },
            "DOM.getDocument"=> {
                if state.switch_during_collection {state.selected=false;}
                if let Some(document) = &state.dom_document { return MockReply::ok(document.clone()); }
                if state.single_frame {
                    return MockReply::ok(json!({"root":{"nodeType":9,"nodeId":100,"backendNodeId":100,"frameId":"F_MAIN","childNodeCount":1,
                        "children":[element(12,12,"INPUT",json!(["data-testid","live-value"]),vec![])]}}));
                }
                if call.session_id.as_deref()==Some("child") {
                    json!({"root":{"nodeType":9,"nodeId":300,"backendNodeId":300,"frameId":"O_FRAME","childNodeCount":1,
                        "children":[element(30,10,"BUTTON",json!(["id","cross-btn","aria-label","Cross"]),vec![])]}})
                } else {document(state.detached)}
            },
            "DOM.describeNode"=>if state.detached || state.node_detached || (state.hit_resolving && state.hit_description_fault=="stale") {return MockReply::err(-32000,"No node with given id")} else {
                let mut node=json!({"backendNodeId":call.params["backendNodeId"],"nodeId":30,"nodeType":1,"nodeName":"BUTTON"});
                if state.hit_resolving {
                    match state.hit_description_fault.as_str() {
                        "ua"=>node["shadowRootType"]=json!("user-agent"),
                        "pseudo"=>node["pseudoType"]=json!("before"),
                        "frame"=>node["nodeName"]=json!("IFRAME"),
                        _=>{}
                    }
                }
                json!({"node":node})
            },
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
            "DOMSnapshot.captureSnapshot"=>if let Some(layout)=&state.dom_layout {layout.clone()} else if state.bad_metadata {json!({})} else if state.single_frame {
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
async fn author_dom_scope_reports_ua_exclusions_without_admitting_internal_query_roots_or_frames() {
    let (f, target, tab) = bound().await;
    let (document, layout) = author_form_fixture();
    {
        let mut state = f.state.lock().unwrap();
        state.single_frame = true;
        state.dom_document = Some(document);
        state.dom_layout = Some(layout);
    }
    let mut request = args(&target, &tab);
    request["include_hidden"] = json!(true);
    request["locator"] = json!({"css":"button[data-testid='save']:not([disabled])"});
    let result = structured(
        &GetBrowserStateTool::new(f.engine.clone())
            .invoke(request)
            .await,
    );
    assert_eq!(
        result["snapshot"]["collection_scope"],
        "author_dom_with_open_shadow"
    );
    assert_eq!(result["snapshot"]["collection_complete"], true, "{result}");
    assert_eq!(result["snapshot"]["omitted"]["nodes"], 0);
    assert_eq!(result["snapshot"]["omitted"]["frames"], 0);
    assert_eq!(
        result["snapshot"]["excluded"],
        json!({"user_agent_shadow_roots":5,
        "user_agent_shadow_hosts":{"input":1,"select":1,"option":3,"textarea":0,"button":0,"other":0}})
    );
    assert_eq!(result["elements"].as_array().unwrap().len(), 1);
    assert_eq!(result["elements"][0]["attributes"]["id"], "main-btn");
    assert!(!result.to_string().contains("PRIVATE_UA_FRAME"));
    let state = f.state.lock().unwrap();
    let roots = state
        .calls
        .iter()
        .filter(|(method, _)| method == "DOM.querySelectorAll")
        .map(|(_, params)| params["nodeId"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(roots, vec![100, 150]);
    assert!(!state
        .calls
        .iter()
        .any(|(method, _)| method.starts_with("Input.")));
}

#[tokio::test]
async fn readonly_actionability_uses_the_attested_world_without_dispatching_input() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .as_str()
        .unwrap();
    let sample = f
        .engine
        .inspect_dom_actionability("dom-inspection-tests", &target, &tab, reference)
        .await
        .unwrap();
    assert_eq!(sample.target.frame_id, "F_MAIN");
    assert_eq!(sample.target.loader_id, "L_MAIN");
    assert_eq!(sample.target.backend_node_id, 10);
    assert!(sample.connected);
    assert_eq!(sample.visible, Some(true));
    assert_eq!(sample.enabled, Some(true));
    assert_eq!(sample.editable, Some(false));
    assert_eq!(sample.receives_pointer, Some(true));
    assert_eq!(sample.bounds, Some([10.5, 20.25, 80.0, 30.0]));
    let state = f.state.lock().unwrap();
    assert!(state
        .calls
        .iter()
        .any(|(method, params)| method == "Page.createIsolatedWorld"
            && params["worldName"] == "cua-driver-dom-actionability"));
    assert!(state
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.releaseObjectGroup"));
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
async fn readonly_actionability_refuses_invalid_state_source_context_and_cleanup() {
    for fault in [
        "malformed",
        "exception",
        "detached",
        "navigation",
        "context",
        "cleanup",
    ] {
        let (f, target, tab) = bound().await;
        let tool = GetBrowserStateTool::new(f.engine.clone());
        let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
        let reference = snapshot["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["attributes"]["id"] == "main-btn")
            .unwrap()["ref"]
            .as_str()
            .unwrap();
        {
            let mut state = f.state.lock().unwrap();
            match fault {
                "malformed" => {
                    state.actionability_reply = Some(
                        json!({"result":{"type":"object","value":{"connected":true,"same_document":true}}}),
                    )
                }
                "exception" => state.geometry_exception = true,
                "detached" => state.geometry_detaches = true,
                "navigation" => state.geometry_navigates = true,
                "context" => state.geometry_context_fault = "frame".into(),
                "cleanup" => state.geometry_cleanup_fails = true,
                _ => unreachable!(),
            }
        }
        let error = f
            .engine
            .inspect_dom_actionability("dom-inspection-tests", &target, &tab, reference)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            if matches!(fault, "detached" | "navigation") {
                BrowserRefusalCode::BrowserRefStale
            } else {
                BrowserRefusalCode::BrowserRouteUnavailable
            },
            "{fault}: {error:?}"
        );
        let state = f.state.lock().unwrap();
        assert!(
            state
                .calls
                .iter()
                .any(|(method, _)| method == "Runtime.releaseObjectGroup"),
            "{fault}"
        );
        assert!(!state
            .calls
            .iter()
            .any(|(method, _)| method.starts_with("Input.") || method.starts_with("Emulation.")));
        if fault == "context" {
            assert!(!state
                .calls
                .iter()
                .any(|(method, _)| method == "Runtime.callFunctionOn"));
        }
    }
}

#[tokio::test]
async fn cancelled_readonly_actionability_still_releases_only_its_object_group() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .as_str()
        .unwrap();
    let resolved = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = f.state.lock().unwrap();
        state.geometry_resolved = Some(resolved.clone());
        state.geometry_released = Some(released.clone());
    }
    {
        let operation =
            f.engine
                .inspect_dom_actionability("dom-inspection-tests", &target, &tab, reference);
        tokio::pin!(operation);
        tokio::select! { biased;
            _ = resolved.notified() => {},
            _ = &mut operation => panic!("actionability completed before cancellation seam"),
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), released.notified())
        .await
        .expect("cancelled actionability must request bounded owned cleanup");
    assert!(!f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.callFunctionOn" || method.starts_with("Input.")));
}

#[tokio::test]
async fn child_frame_actionability_refuses_without_evaluating_a_main_world() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "inner-btn")
        .unwrap()["ref"]
        .as_str()
        .unwrap();
    let count = f.state.lock().unwrap().calls.len();
    let error = f
        .engine
        .inspect_dom_actionability("dom-inspection-tests", &target, &tab, reference)
        .await
        .unwrap_err();
    assert_eq!(error.code, BrowserRefusalCode::BrowserRouteUnavailable);
    assert_eq!(f.state.lock().unwrap().calls.len(), count);
}
#[tokio::test]
async fn explicit_geometry_reads_current_ref_without_minting_pixel_authority() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .clone();
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference.clone();
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["mode"], "element_geometry", "{result}");
    assert_eq!(result["ref"], reference);
    assert_eq!(result["coordinate_space"], "frame_viewport_css_pixels");
    assert_eq!(result["method"], "isolated_cssom_border_box");
    assert_eq!(result["native_pixel_mapping"], "unavailable");
    assert_eq!(result["bounds"]["x"], 10.5);
    assert_eq!(result["scroll"]["y"], 120.0);
    assert!(f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.releaseObjectGroup"));
}

#[tokio::test]
async fn cssom_fractional_geometry_preserves_exact_cdp_float64_values() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .clone();
    f.state.lock().unwrap().fractional_geometry = true;
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference;
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["mode"], "element_geometry", "{result}");
    for (field, expected) in [
        ("y", 182.89584350585938_f64),
        ("width", 241.33334350585938_f64),
    ] {
        let actual = result["bounds"][field]
            .as_f64()
            .expect("finite CSSOM coordinate");
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "CDP JSON must preserve {field} exactly: {actual} versus {expected}"
        );
    }
}

#[tokio::test]
async fn geometry_fresh_attachment_sessions_do_not_change_the_bound_source() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().distinct_attachment_ids = true;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .clone();
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference.clone();
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["mode"], "element_geometry", "{result}");
    assert_eq!(result["source_ref"], reference);
    assert_eq!(result["native_pixel_mapping"], "unavailable");
}

#[tokio::test]
async fn geometry_open_shadow_ref_keeps_main_frame_and_readonly_authority() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let row = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["data-testid"] == "shadow")
        .unwrap();
    let mut request = args(&target, &tab);
    request["geometry_ref"] = row["ref"].clone();
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["mode"], "element_geometry", "{result}");
    assert_eq!(result["frame"]["kind"], row["frame"]["kind"]);
    assert_eq!(result["source_ref"], row["ref"]);
    assert_eq!(result["native_pixel_mapping"], "unavailable");
    let calls = &f.state.lock().unwrap().calls;
    assert!(calls
        .iter()
        .any(|(method, params)| method == "DOM.resolveNode" && params["backendNodeId"] == 15));
    assert!(!calls.iter().any(|(method, _)| method.starts_with("Input.")
        || matches!(
            method.as_str(),
            "DOM.focus"
                | "DOM.scrollIntoViewIfNeeded"
                | "Page.captureScreenshot"
                | "Target.activateTarget"
                | "Page.bringToFront"
        )));
}

#[tokio::test]
async fn cancelled_geometry_still_releases_its_remote_object_group() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attributes"]["id"] == "main-btn")
        .unwrap()["ref"]
        .clone();
    let resolved = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = f.state.lock().unwrap();
        state.geometry_resolved = Some(resolved.clone());
        state.geometry_released = Some(released.clone());
    }
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference;
    {
        let operation = tool.invoke(request);
        tokio::pin!(operation);
        tokio::select! {
            biased;
            _ = resolved.notified() => {},
            _ = &mut operation => panic!("geometry completed before cancellation seam"),
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), released.notified())
        .await
        .expect("cancelled geometry must request bounded owned cleanup");
    assert!(!f
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|(method, _)| method == "Runtime.callFunctionOn"));
}

#[tokio::test]
async fn geometry_context_identity_must_be_unique_and_frame_attested() {
    for fault in [
        "session",
        "numeric_id",
        "name",
        "frame",
        "default_world",
        "reused_id",
    ] {
        let (f, target, tab) = bound().await;
        let tool = GetBrowserStateTool::new(f.engine.clone());
        let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
        let reference = snapshot["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["attributes"]["id"] == "main-btn")
            .unwrap()["ref"]
            .clone();
        f.state.lock().unwrap().geometry_context_fault = fault.into();
        let mut request = args(&target, &tab);
        request["geometry_ref"] = reference;
        let result = structured(&tool.invoke(request).await);
        assert_eq!(
            result["refusal"]["code"], "browser_route_unavailable",
            "{fault}: {result}"
        );
        let state = f.state.lock().unwrap();
        assert!(
            !state.calls.iter().any(
                |(method, _)| method == "DOM.resolveNode" || method == "Runtime.callFunctionOn"
            ),
            "{fault}"
        );
        assert!(
            state
                .calls
                .iter()
                .any(|(method, _)| method == "Runtime.releaseObjectGroup"),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn geometry_context_exception_navigation_and_cleanup_fail_closed() {
    for fault in [
        "missing_unique",
        "exception",
        "detached",
        "navigation",
        "cleanup",
    ] {
        let (f, target, tab) = bound().await;
        let tool = GetBrowserStateTool::new(f.engine.clone());
        let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
        let reference = snapshot["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["attributes"]["id"] == "main-btn")
            .unwrap()["ref"]
            .clone();
        {
            let mut state = f.state.lock().unwrap();
            state.geometry_missing_unique = fault == "missing_unique";
            state.geometry_exception = fault == "exception";
            state.geometry_detaches = fault == "detached";
            state.geometry_navigates = fault == "navigation";
            state.geometry_cleanup_fails = fault == "cleanup";
        }
        let mut request = args(&target, &tab);
        request["geometry_ref"] = reference;
        let result = structured(&tool.invoke(request).await);
        assert_eq!(
            result["refusal"]["code"],
            if matches!(fault, "navigation" | "detached") {
                "browser_ref_stale"
            } else {
                "browser_route_unavailable"
            },
            "{fault}: {result}"
        );
        let state = f.state.lock().unwrap();
        if fault == "missing_unique" {
            assert!(!state.calls.iter().any(
                |(method, _)| method == "DOM.resolveNode" || method == "Runtime.callFunctionOn"
            ));
        }
        assert!(
            state
                .calls
                .iter()
                .any(|(method, _)| method == "Runtime.releaseObjectGroup"),
            "{fault}"
        );
        assert!(!state
            .calls
            .iter()
            .any(|(method, _)| method.starts_with("Input.")
                || matches!(
                    method.as_str(),
                    "DOM.focus"
                        | "DOM.scrollIntoViewIfNeeded"
                        | "Target.activateTarget"
                        | "Page.bringToFront"
                )));
    }
}

#[tokio::test]
async fn geometry_child_and_stale_refs_refuse_without_rebinding() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let snapshot = structured(&tool.invoke(args(&target, &tab)).await);
    let reference = snapshot["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["frame"]["kind"] == "iframe")
        .unwrap()["ref"]
        .clone();
    let before = f.state.lock().unwrap().calls.len();
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference;
    let result = structured(&tool.invoke(request).await);
    assert_eq!(
        result["refusal"]["code"], "browser_route_unavailable",
        "{result}"
    );
    assert!(!f.state.lock().unwrap().calls[before..]
        .iter()
        .any(|(method, _)| method == "DOM.resolveNode"));
    let reference = snapshot["elements"][0]["ref"].clone();
    tool.invoke(args(&target, &tab)).await;
    let mut request = args(&target, &tab);
    request["geometry_ref"] = reference;
    let result = structured(&tool.invoke(request).await);
    assert_eq!(result["refusal"]["code"], "browser_ref_stale", "{result}");
}

#[tokio::test]
async fn geometry_rejects_mixed_or_malformed_requests_before_cdp() {
    let (f, target, tab) = bound().await;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    for extra in [
        json!({"geometry_ref":""}),
        json!({"geometry_ref":1}),
        json!({"geometry_ref":"p1:0","scope_ref":"p1:0"}),
        json!({"geometry_ref":"p1:0","max_nodes":10}),
        json!({"geometry_ref":"p1:0","snapshot_format":"semantic_v2"}),
    ] {
        let mut request = args(&target, &tab);
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let before = f.state.lock().unwrap().calls.len();
        assert!(tool.invoke(request).await.is_error.unwrap_or(false));
        assert_eq!(f.state.lock().unwrap().calls.len(), before);
    }
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
async fn unqualified_layout_units_never_become_css_geometry() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().single_frame = true;
    let result = structured(
        &GetBrowserStateTool::new(f.engine.clone())
            .invoke(args(&target, &tab))
            .await,
    );
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["snapshot"]["collection_complete"], true);
    assert_eq!(result["snapshot"]["complete"], false, "{result}");
    assert_eq!(result["snapshot"]["geometry_complete"], false);
    assert_eq!(result["elements"].as_array().unwrap().len(), 1);
    let element = &result["elements"][0];
    assert_eq!(element["value"], "live-value");
    assert_eq!(element["states"]["checked"], true);
    assert_eq!(element["bounds_css"], Value::Null);
    assert_eq!(
        element["geometry_status"],
        "unqualified_native_layout_units"
    );
}

#[tokio::test]
async fn missing_layout_identity_cannot_mint_a_complete_readonly_record() {
    let (f, target, tab) = bound().await;
    f.state.lock().unwrap().single_frame = true;
    let tool = GetBrowserStateTool::new(f.engine.clone());
    let baseline = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(
        baseline["snapshot"]["metadata_complete"], true,
        "{baseline}"
    );
    assert_eq!(baseline["elements"].as_array().unwrap().len(), 1);
    f.state.lock().unwrap().missing_layout_identity = true;
    let result = structured(&tool.invoke(args(&target, &tab)).await);
    assert_eq!(result["snapshot"]["metadata_complete"], false, "{result}");
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
    assert_eq!(
        baseline["snapshot"]["metadata_complete"], true,
        "{baseline}"
    );
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
    assert_eq!(result["snapshot"]["metadata_complete"], false);
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
    assert_eq!(cross["bounds_css"], Value::Null);
    assert_eq!(cross["geometry_status"], "unqualified_native_layout_units");
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
        .all(|e| e["bounds_css"].is_null()
            && e["geometry_status"] == "unqualified_native_layout_units"));
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
