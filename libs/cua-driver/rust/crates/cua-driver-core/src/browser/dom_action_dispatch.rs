//! Explicit programmatic DOM dispatch. Never a trusted-input or coordinate fallback.
use super::*;
use crate::browser::dom_action_operation::{self as operation, DomActionStep};
use crate::browser::dom_action_progress::{DomActionCancellation, DomActionProgress};
use crate::browser::dom_action_request::DomActionRequest;
use crate::browser::dom_action_wait::ActionRequirements;

const WORLD: &str = "cua-driver-dom-actions";

/// The caller must select the route explicitly; unsupported trusted input never downgrades.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DomActionRoute {
    Trusted,
    DomEvent,
}

fn refusal(reason: &'static str) -> BrowserRefusal {
    refuse(BrowserRefusalCode::BrowserActionUnavailable, reason)
}

impl BrowserEngine {
    /// Execute an independently authorized mutating intent, not an inspection-ref capability.
    /// This internal entry point is not a public tool or permission adapter. Its caller must
    /// separately authorize the intent. Synthetic/programmatic acknowledgements do not prove
    /// trusted input, CSS hover, browser key defaults, application success or atomic paint.
    pub(crate) async fn dispatch_dom_action(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        source_ref: &str,
        request: &DomActionRequest,
        route: DomActionRoute,
        progress: DomActionProgress,
    ) -> Result<Value, BrowserRefusal> {
        let mut cancellation = DomActionCancellation::new(progress.clone());
        if route != DomActionRoute::DomEvent {
            progress.finish("refused", Some("dom_action_trusted_route_unavailable"));
            cancellation.disarm();
            return Err(refuse(
                BrowserRefusalCode::BrowserInputTrustUnavailable,
                "this bounded no-coordinate route requires explicit dom_event intent",
            )
            .with_detail(progress.snapshot()));
        }
        let result = tokio::time::timeout(Duration::from_millis(request.timeout_ms), async {
            let prepared = self.prepare_dom_action(session,target_id,tab_id,source_ref,request).await?;
            let steps = operation::plan(request,prepared.press_key.as_ref()).map_err(refusal)?;
            let interval = self.lock_mutation(session,target_id,tab_id).await?;
            let DomNodeSample::Actionability(current) = self.sample_dom_ref(session,target_id,tab_id,
                &prepared.reference,DomNodeSampleKind::Actionability).await? else {
                    return Err(refusal("dom_action_state_unavailable"));
                };
            let required = super::super::dom_action_engine::requirements(request.kind);
            check_ready(&current,&prepared.state,required)?;
            let entry = self.store.resolve_dom_ref(session,target_id,tab_id,&prepared.reference)?;
            let validated = self.revalidate_for_mutation(session,target_id,Some(tab_id)).await?;
            let mut lease = ActionLease::new(self,session,target_id,tab_id,validated,entry.frame.clone(),
                prepared.state.target.clone(),progress.clone(),interval);
            // This cleanup-owning lease exists before any remote object can be allocated.
            let sampled = async {
                lease.check_source().await?;
                read(&lease.validated.conn,&lease.validated.cdp_session,"Runtime.enable",json!({})).await?;
                let world = read(&lease.validated.conn,&lease.validated.cdp_session,"Page.createIsolatedWorld",
                    json!({"frameId":lease.target.frame_id,"worldName":WORLD,"grantUniveralAccess":false})).await?;
                lease.evidence.id=world["executionContextId"].as_i64().filter(|id|*id>0)
                    .ok_or_else(||refusal("dom_action_context_unavailable"))?;
                lease.drain()?;
                lease.unique=lease.evidence.unique_id.clone().ok_or_else(||refusal("dom_action_context_unavailable"))?;
                let resolved=read_node(&lease.validated.conn,&lease.validated.cdp_session,"DOM.resolveNode",
                    json!({"backendNodeId":lease.target.backend_node_id,
                        "executionContextId":lease.evidence.id,"objectGroup":lease.group})).await?;
                lease.object=resolved.pointer("/object/objectId").and_then(Value::as_str)
                    .filter(|id|!id.is_empty()&&id.len()<=1024).ok_or_else(||refusal("dom_action_node_unavailable"))?.to_owned();
                lease.drain()?;
                let data=json!({"action":action_name(request.kind),"text":request.text,
                    "values":request.values,"key":request.key});
                let reply=lease.call(operation::PREFLIGHT_FUNCTION,data,true).await?;
                project_reply(&reply)?;
                let mut observed=json!([]);
                for step in &steps {
                    // Every effect remains tied to the same node/context, even after earlier
                    // handlers run. No fresh locator lookup, retargeting or replay is allowed.
                    lease.check_source().await?;
                    let sampled=lease.call(dom_action_sample::READ_FUNCTION,Value::Null,true).await?;
                    let mut state=dom_action_sample::parse_sample(&sampled,lease.target.clone()).map_err(refusal)?;
                    AttestedHit {
                        conn: &lease.validated.conn,
                        session: &lease.validated.cdp_session,
                        frame_id: &lease.target.frame_id,
                        context_id: lease.evidence.id,
                        unique_id: &lease.unique,
                        group: &lease.group,
                        object_id: &lease.object,
                    }.qualify_pointer(&sampled,&mut state,&mut lease.evidence,&mut lease.events).await?;
                    lease.check_source().await?;
                    check_ready(&state,&current,required)?;
                    if step.name=="dwell" {
                        let ms=step.argument.as_u64().filter(|ms|*ms<=10_000)
                            .ok_or_else(||refusal("dom_action_invalid_dwell"))?;
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        continue;
                    }
                    // Native object/context ownership is retained for a possible synthetic key-up.
                    if step.name=="keydown" {
                        lease.keyup=Some(json!({"name":"keyup","argument":step.argument}));
                    }
                    progress.begin_step(step.name).map_err(refusal)?;
                    lease.attempted=true;
                    self.store.invalidate_tab_snapshots(session,target_id,tab_id);
                    let reply=lease.call(operation::STEP_FUNCTION,step_value(step),false).await?;
                    if reply.pointer("/result/value/ok").and_then(Value::as_bool).is_none() {
                        return Err(refusal("dom_action_reply_unavailable"));
                    }
                    progress.acknowledge_step(step.name).map_err(refusal)?;
                    let fields=project_reply(&reply)?;
                    observed.as_array_mut().expect("bounded observation list")
                        .push(json!({"step":step.name,"observed":fields}));
                }
                lease.check_source().await?;
                Ok(observed)
            }.await;
            let cleanup=lease.release().await;
            match (sampled,cleanup) {
                (Err(error),_)=>Err(error),
                (Ok(_),false)=>Err(refusal("dom_action_cleanup_unconfirmed")),
                (Ok(observed),true)=>Ok(observed),
            }
        }).await;
        let result = match result {
            Ok(result) => result,
            Err(_) => Err(refusal("dom_action_timeout")),
        };
        cancellation.disarm();
        match result {
            Ok(observed) => {
                progress.finish("dispatched", None);
                Ok(
                    json!({"status":"dispatched","input_route":"dom_event","trusted":false,
                    "effect":"unverified","observed":observed,"progress":progress.snapshot(),
                    "native_pixel_mapping":"unavailable","atomic":false,
                    "limitations":["programmatic_dom_semantics_only","no_css_hover_proof",
                        "no_trusted_key_defaults","no_application_success_proof"]}),
                )
            }
            Err(error) => {
                let reason = crate::browser::dom_action_reason::progress_reason(&error.message);
                let status = if reason == "dom_action_timeout" {
                    "timeout"
                } else if progress.snapshot()["in_flight"].is_null() {
                    "refused"
                } else {
                    "unknown"
                };
                progress.finish(status, Some(reason));
                Err(
                    error.with_detail(json!({"input_route":"dom_event","trusted":false,
                    "progress":progress.snapshot(),"automatic_retry":false})),
                )
            }
        }
    }
}

fn step_value(step: &DomActionStep) -> Value {
    json!({"name":step.name,"argument":step.argument})
}
fn action_name(kind: crate::browser::dom_action_request::DomActionKind) -> &'static str {
    use crate::browser::dom_action_request::DomActionKind::*;
    match kind {
        Click => "click",
        DoubleClick => "double_click",
        RightClick => "right_click",
        Hover => "hover",
        Fill => "fill",
        Type => "type",
        Press => "press",
        Check => "check",
        Uncheck => "uncheck",
        SelectOption => "select_option",
        Focus => "focus",
        ScrollIntoView => "scroll_into_view",
    }
}
fn check_ready(
    state: &ActionState,
    expected: &ActionState,
    required: ActionRequirements,
) -> Result<(), BrowserRefusal> {
    if state.target != expected.target || !state.connected {
        return Err(refusal("dom_action_source_changed"));
    }
    if (required.visible && state.visible != Some(true))
        || (required.enabled && state.enabled != Some(true))
        || (required.editable && state.editable != Some(true))
        || (required.hit_test && state.receives_pointer != Some(true))
        || (required.stable && state.bounds != expected.bounds)
    {
        return Err(refusal("dom_action_readiness_changed"));
    }
    Ok(())
}
fn project_reply(reply: &Value) -> Result<Value, BrowserRefusal> {
    let value = reply
        .pointer("/result/value")
        .filter(|v| v.is_object())
        .ok_or_else(|| refusal("dom_action_reply_unavailable"))?;
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => {
            return Err(refusal(match value["reason"].as_str() {
                Some("dom_action_ref_stale") => "dom_action_ref_stale",
                Some("dom_action_control_unavailable") => "dom_action_control_unavailable",
                Some("dom_action_option_unavailable") => "dom_action_option_unavailable",
                Some("dom_action_not_focused") => "dom_action_not_focused",
                Some("dom_action_postcondition_unconfirmed") => {
                    "dom_action_postcondition_unconfirmed"
                }
                Some("dom_action_event_cancelled") => "dom_action_event_cancelled",
                _ => "dom_action_operation_refused",
            }))
        }
        None => return Err(refusal("dom_action_reply_unavailable")),
    }
    let mut observed = serde_json::Map::new();
    if let Some(fields) = value.get("observed").and_then(Value::as_object) {
        for field in [
            "activation_invoked",
            "focused",
            "scroll_requested",
            "already_matched",
            "checked_matches",
            "value_matches",
            "text_applied",
            "selected_matches",
            "event_cancelled",
        ] {
            if let Some(v) = fields.get(field) {
                observed.insert(
                    field.to_owned(),
                    json!(v
                        .as_bool()
                        .ok_or_else(|| refusal("dom_action_reply_unavailable"))?),
                );
            }
        }
    }
    Ok(Value::Object(observed))
}

struct ActionLease<'a> {
    engine: &'a BrowserEngine,
    session: &'a str,
    target_id: &'a str,
    tab_id: &'a str,
    validated: ValidatedTab,
    frame: FrameRef,
    target: ActionTarget,
    events: BoundedCdpEvents,
    evidence: ContextEvidence,
    unique: String,
    object: String,
    group: String,
    keyup: Option<Value>,
    progress: DomActionProgress,
    attempted: bool,
    armed: bool,
    interval: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl<'a> ActionLease<'a> {
    fn new(
        engine: &'a BrowserEngine,
        session: &'a str,
        target_id: &'a str,
        tab_id: &'a str,
        validated: ValidatedTab,
        frame: FrameRef,
        target: ActionTarget,
        progress: DomActionProgress,
        interval: tokio::sync::OwnedMutexGuard<()>,
    ) -> Self {
        let events = validated.conn.subscribe_bounded(EVENT_LIMIT, REPLY_LIMIT);
        Self {
            engine,
            session,
            target_id,
            tab_id,
            validated,
            frame,
            target,
            events,
            evidence: ContextEvidence {
                id: 0,
                world_name: WORLD,
                unique_id: None,
                consumed: 0,
            },
            unique: String::new(),
            object: String::new(),
            group: format!("cua-dom-action-{}", Uuid::new_v4()),
            keyup: None,
            progress,
            attempted: false,
            armed: true,
            interval: Some(interval),
        }
    }
    fn drain(&mut self) -> Result<(), BrowserRefusal> {
        self.evidence.drain(
            &mut self.events,
            &self.validated.cdp_session,
            &self.target.frame_id,
        )
    }
    async fn check_source(&mut self) -> Result<(), BrowserRefusal> {
        let current = self
            .engine
            .revalidate_for_mutation(self.session, self.target_id, Some(self.tab_id))
            .await?;
        if !Arc::ptr_eq(&current.conn, &self.validated.conn)
            || current.tab.cdp_target_id != self.validated.tab.cdp_target_id
            || current.record.generation != self.validated.record.generation
            || current.tab.generation != self.validated.tab.generation
            || current.record.fingerprint != self.validated.record.fingerprint
            || current.record.window_id != self.validated.record.window_id
        {
            return Err(refusal("dom_action_source_changed"));
        }
        self.engine
            .frame_session_for_mutation(
                self.session,
                self.target_id,
                self.tab_id,
                &current,
                &self.frame,
            )
            .await?;
        describe_source(
            &current.conn,
            &current.cdp_session,
            self.target.backend_node_id,
        )
        .await?;
        self.engine
            .require_same_selected_dom_target(&current)
            .await?;
        self.drain()
    }
    async fn call(
        &mut self,
        function: &str,
        data: Value,
        readonly: bool,
    ) -> Result<Value, BrowserRefusal> {
        self.drain()?;
        let arguments = if function == dom_action_sample::READ_FUNCTION {
            json!([{"objectId":self.object}])
        } else {
            json!([{"objectId":self.object},{"value":data}])
        };
        // Transfer the sole normal key-up attempt only after the final evidence check.
        if function == operation::STEP_FUNCTION && data["name"] == "keyup" {
            self.keyup = None;
        }
        let reply=read(&self.validated.conn,&self.validated.cdp_session,"Runtime.callFunctionOn",
            json!({"functionDeclaration":function,"uniqueContextId":self.unique,"arguments":arguments,
                "returnByValue":true,"awaitPromise":false,"userGesture":false,"throwOnSideEffect":readonly})).await?;
        self.drain()?;
        Ok(reply)
    }
    async fn release(&mut self) -> bool {
        // Transfer ownership before awaiting. Dropping this waiter cannot replay cleanup.
        self.armed = false;
        self.start_cleanup().await.unwrap_or(false)
    }
    fn start_cleanup(&mut self) -> tokio::task::JoinHandle<bool> {
        spawn_cleanup(
            self.validated.conn.clone(),
            self.validated.cdp_session.clone(),
            self.group.clone(),
            self.unique.clone(),
            self.object.clone(),
            self.keyup.take(),
            self.progress.clone(),
            self.interval.take(),
        )
    }
}
impl Drop for ActionLease<'_> {
    fn drop(&mut self) {
        if self.attempted {
            self.engine
                .store
                .invalidate_tab_snapshots(self.session, self.target_id, self.tab_id);
        }
        if !self.armed {
            return;
        }
        self.armed = false;
        self.progress.cleanup(false);
        if tokio::runtime::Handle::try_current().is_ok() {
            // Exactly one supervisor owns this cleanup, even when its waiter disappears.
            drop(self.start_cleanup());
        }
    }
}
fn spawn_cleanup(
    conn: Arc<CdpConnection>,
    session: String,
    group: String,
    unique: String,
    object: String,
    keyup: Option<Value>,
    progress: DomActionProgress,
    interval: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> tokio::task::JoinHandle<bool> {
    progress.cleanup(false);
    tokio::spawn(async move {
        let confirmed = cleanup(&conn, &session, &group, &unique, &object, keyup).await;
        progress.cleanup(confirmed);
        // Retain real-target serialization even if the action's caller was cancelled.
        drop(interval);
        confirmed
    })
}
async fn cleanup(
    conn: &CdpConnection,
    session: &str,
    group: &str,
    unique: &str,
    object: &str,
    keyup: Option<Value>,
) -> bool {
    let key_released = if let Some(data) = keyup {
        // Only the original unique world/node is used, once. A lost reply stays unknown.
        match tokio::time::timeout(
            Duration::from_millis(500),
            read(
                conn,
                session,
                "Runtime.callFunctionOn",
                json!({"functionDeclaration":operation::STEP_FUNCTION,"uniqueContextId":unique,
                "arguments":[{"objectId":object},{"value":data}],"returnByValue":true,
                "awaitPromise":false,"userGesture":false,"throwOnSideEffect":false}),
            ),
        )
        .await
        {
            Ok(Ok(reply)) => project_reply(&reply).is_ok(),
            _ => false,
        }
    } else {
        true
    };
    // Independent deadline: a blocked script/dialog must not skip handle release.
    let group_released = release_group(conn, session, group).await.is_ok();
    key_released && group_released
}
