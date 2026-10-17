//! Attested readonly CSSOM samples and an explicitly requested native candidate.

use super::super::cdp_ws::{BoundedCdpEvents, BoundedEventError};
use super::super::dom_action_hit::{self, HitProbe};
use super::super::dom_action_sample;
use super::super::dom_action_wait::{ActionState, ActionTarget};
use super::super::dom_cssom_sample;
use super::super::{dom_geometry::map_layout_bounds, dom_native_sample};
use super::*;
use std::time::Duration;

#[path = "dom_action_dispatch.rs"]
pub(crate) mod dispatch;

const REPLY_LIMIT: usize = 16 * 1024;
const EVENT_LIMIT: usize = 512;

/// Only these native readonly functions can cross the attested-world boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DomNodeSampleKind {
    Geometry,
    NativeGeometry,
    Actionability,
}

impl DomNodeSampleKind {
    fn world(self) -> &'static str {
        match self {
            Self::Geometry => dom_cssom_sample::WORLD_NAME,
            Self::NativeGeometry => dom_native_sample::WORLD_NAME,
            Self::Actionability => dom_action_sample::WORLD_NAME,
        }
    }

    fn function(self) -> &'static str {
        match self {
            Self::Geometry => dom_cssom_sample::READ_FUNCTION,
            Self::NativeGeometry => dom_native_sample::READ_FUNCTION,
            Self::Actionability => dom_action_sample::READ_FUNCTION,
        }
    }

    fn parse(self, reply: &Value, target: ActionTarget) -> Result<DomNodeSample, BrowserRefusal> {
        match self {
            Self::Geometry => dom_cssom_sample::parse_sample(reply)
                .map(DomNodeSample::Geometry)
                .map_err(|reason| {
                    if reason == "dom_geometry_source_detached" {
                        stale("geometry source detached or changed document")
                    } else {
                        unavailable("fixed side-effect-checked geometry sample is unavailable")
                    }
                }),
            Self::NativeGeometry => dom_native_sample::parse_sample(reply)
                .map(|sample| DomNodeSample::NativeGeometry(sample.0))
                .map_err(|reason| {
                    if reason == "dom_geometry_source_detached" {
                        stale("native geometry source detached or changed document")
                    } else {
                        unavailable(reason)
                    }
                }),
            Self::Actionability => dom_action_sample::parse_sample(reply, target)
                .map(DomNodeSample::Actionability)
                .map_err(|reason| {
                    if reason == "dom_action_ref_stale" {
                        stale("actionability source detached or changed document")
                    } else {
                        unavailable("dom_action_state_unavailable")
                    }
                }),
        }
    }
}

enum DomNodeSample {
    Geometry(Value),
    NativeGeometry(Value),
    Actionability(ActionState),
}

impl BrowserEngine {
    /// Sample a current main-document inspection ref in an attested isolated world.
    /// This neither mints authority nor qualifies native pixels or atomic paint.
    pub(crate) async fn inspect_dom_geometry(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        reference: &str,
    ) -> Result<Value, BrowserRefusal> {
        let _interval = self.lock_mutation(session, target_id, tab_id).await?;
        let DomNodeSample::Geometry(sample) = self
            .sample_dom_ref(
                session,
                target_id,
                tab_id,
                reference,
                DomNodeSampleKind::Geometry,
            )
            .await?
        else {
            return Err(unavailable("readonly sample kind changed"));
        };
        let mut output = json!({
            "status":"ok","mode":"element_geometry","ref":reference,
            "source_ref":reference,"target_id":target_id,"tab_id":tab_id,
            "frame":{"kind":"main"},"coordinate_space":"frame_viewport_css_pixels",
            "method":"isolated_cssom_border_box","viewport":"layout","native_pixel_mapping":"unavailable",
            "atomic":false
        });
        for field in [
            "connected",
            "same_document",
            "bounds",
            "scroll",
            "visual_viewport",
        ] {
            output[field] = sample[field].clone();
        }
        Ok(output)
    }

    /// Project one main-document ref with a directly measured native RootWebArea.
    /// This opt-in candidate is not paint-atomic, raster-qualified or input authority.
    pub(crate) async fn inspect_dom_native_geometry(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        reference: &str,
    ) -> Result<Value, BrowserRefusal> {
        let _interval = self.lock_mutation(session, target_id, tab_id).await?;
        match self
            .sample_dom_ref(
                session,
                target_id,
                tab_id,
                reference,
                DomNodeSampleKind::NativeGeometry,
            )
            .await?
        {
            DomNodeSample::NativeGeometry(sample) => Ok(sample),
            _ => Err(unavailable("readonly sample kind changed")),
        }
    }
    /// Read actionability from a current inspection ref without granting action authority.
    /// This samples only the main/open-shadow document; dispatch must independently revalidate.
    pub(crate) async fn inspect_dom_actionability(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        reference: &str,
    ) -> Result<ActionState, BrowserRefusal> {
        let _interval = self.lock_mutation(session, target_id, tab_id).await?;
        match self
            .sample_dom_ref(
                session,
                target_id,
                tab_id,
                reference,
                DomNodeSampleKind::Actionability,
            )
            .await?
        {
            DomNodeSample::Actionability(sample) => Ok(sample),
            DomNodeSample::Geometry(_) | DomNodeSample::NativeGeometry(_) => {
                Err(unavailable("readonly sample kind changed"))
            }
        }
    }

    // Both entry points hold the same real-target serialization interval around this entire read.
    async fn sample_dom_ref(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        reference: &str,
        kind: DomNodeSampleKind,
    ) -> Result<DomNodeSample, BrowserRefusal> {
        let entry = self
            .store
            .resolve_dom_ref(session, target_id, tab_id, reference)
            .map_err(|error| {
                self.store
                    .invalidate_tab_snapshots(session, target_id, tab_id);
                error
            })?;
        let identity = entry
            .frame
            .identity
            .as_ref()
            .filter(|identity| !identity.frame_id.is_empty() && !identity.loader_id.is_empty())
            .ok_or_else(|| {
                self.store
                    .invalidate_tab_snapshots(session, target_id, tab_id);
                stale("geometry requires an attested document")
            })?;
        if matches!(kind, DomNodeSampleKind::NativeGeometry)
            && [&identity.frame_id, &identity.loader_id]
                .iter()
                .any(|id| id.len() > 256 || id.contains('\0'))
        {
            return Err(unavailable(
                "native geometry document identity is not bounded",
            ));
        }
        if entry.frame.kind != FrameKind::Main || entry.frame.oopif_target_id.is_some() {
            return Err(unavailable("child-frame geometry is not qualified"));
        }
        if entry.backend_node_id <= 0 {
            self.store
                .invalidate_tab_snapshots(session, target_id, tab_id);
            return Err(stale("geometry source node is unavailable"));
        }
        let validated = self
            .revalidate_for_mutation(session, target_id, Some(tab_id))
            .await?;
        let prefix = match kind {
            DomNodeSampleKind::Geometry => "cua-dom-geometry",
            DomNodeSampleKind::NativeGeometry => "cua-dom-native-geometry",
            DomNodeSampleKind::Actionability => "cua-dom-actionability",
        };
        let group = format!("{prefix}-{}", Uuid::new_v4());
        let mut objects = RemoteObjectGroup {
            conn: validated.conn.clone(),
            session: validated.cdp_session.clone(),
            name: group.clone(),
            armed: true,
        };
        let mut events = validated.conn.subscribe_bounded(EVENT_LIMIT, REPLY_LIMIT);
        let sampled = tokio::time::timeout(Duration::from_secs(10), async {
            self.require_same_selected_dom_target(&validated).await?;
            let frame_session = self
                .frame_session_for_mutation(session, target_id, tab_id, &validated, &entry.frame)
                .await?;
            if frame_session != validated.cdp_session {
                return Err(stale("geometry source session changed"));
            }
            describe_source(&validated.conn, &frame_session, entry.backend_node_id).await?;
            read(&validated.conn, &frame_session, "Runtime.enable", json!({})).await?;
            let world = read(
                &validated.conn,
                &frame_session,
                "Page.createIsolatedWorld",
                json!({
                    "frameId":identity.frame_id,"worldName":kind.world(),"grantUniveralAccess":false
                }),
            )
            .await?;
            let context_id = world
                .get("executionContextId")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
                .ok_or_else(|| unavailable("isolated geometry context is unavailable"))?;
            let mut evidence = ContextEvidence {
                id: context_id,
                world_name: kind.world(),
                unique_id: None,
                consumed: 0,
            };
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            let unique_id = evidence.unique_id.clone().ok_or_else(|| {
                unavailable("isolated geometry context was not independently attested")
            })?;
            self.frame_session_for_mutation(session, target_id, tab_id, &validated, &entry.frame)
                .await?;
            let resolved = read_node(
                &validated.conn,
                &frame_session,
                "DOM.resolveNode",
                json!({
                    "backendNodeId":entry.backend_node_id,
                    "executionContextId":context_id,"objectGroup":group
                }),
            )
            .await?;
            if resolved.pointer("/object/subtype").and_then(Value::as_str) == Some("null") {
                return Err(stale("geometry source node is unavailable"));
            }
            let object_id = resolved
                .pointer("/object/objectId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 1024)
                .ok_or_else(|| {
                    unavailable("geometry node resolution did not return a bounded object")
                })?;
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            let reply = read(
                &validated.conn,
                &frame_session,
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration":kind.function(),"uniqueContextId":unique_id,
                    "arguments":[{"objectId":object_id}],
                    "returnByValue":true,"awaitPromise":false,"userGesture":false,
                    "throwOnSideEffect":true
                }),
            )
            .await?;
            let target = ActionTarget {
                frame_id: identity.frame_id.clone(),
                loader_id: identity.loader_id.clone(),
                backend_node_id: entry.backend_node_id,
            };
            let mut sample = kind.parse(&reply, target.clone())?;
            if let DomNodeSample::Actionability(state) = &mut sample {
                AttestedHit {
                    conn: &validated.conn,
                    session: &frame_session,
                    frame_id: &identity.frame_id,
                    context_id,
                    unique_id: &unique_id,
                    group: &group,
                    object_id,
                }
                .qualify_pointer(&reply, state, &mut evidence, &mut events)
                .await?;
            }
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            // NativeWindowInfo bounds serve CDP correlation (Chromium DIP on Windows).
            // NativeBrowserViewport bounds are physical pixels. Never compare or fit
            // those spaces: retain DIP continuity separately from first == second below.
            let mut initial_cdp_window = None;
            if let DomNodeSample::NativeGeometry(css) = &sample {
                let css = dom_native_sample::NativeSample(css.clone());
                if !validated.native.geometry_exact {
                    return Err(stale("native CDP-correlation window geometry is not exact"));
                }
                initial_cdp_window = Some(validated.native.bounds);
                let first = self.platform.native_browser_viewport(
                    validated.record.pid, validated.record.window_id,
                ).await?;
                let version = validated.conn.call_bounded(
                    None, "Browser.getVersion", json!({}), REPLY_LIMIT,
                ).await.map_err(|_| unavailable("browser version evidence unavailable"))?;
                let mut browser_version = json!({});
                for field in ["product", "revision"] {
                    let text = version.get(field).and_then(Value::as_str)
                        .filter(|text| !text.is_empty() && text.len() <= 256 && !text.contains('\0'))
                        .ok_or_else(|| unavailable("browser version evidence is not bounded"))?;
                    browser_version[field] = json!(text);
                }
                let second = self.platform.native_browser_viewport(
                    validated.record.pid, validated.record.window_id,
                ).await?;
                if first != second {
                    return Err(stale("native viewport source changed during geometry sample"));
                }
                let after = read(
                    &validated.conn, &frame_session, "Runtime.callFunctionOn",
                    json!({"functionDeclaration":kind.function(),"uniqueContextId":unique_id,
                        "arguments":[{"objectId":object_id}],"returnByValue":true,
                        "awaitPromise":false,"userGesture":false,"throwOnSideEffect":true}),
                ).await?;
                let after = dom_native_sample::parse_sample(&after).map_err(|reason| {
                    if reason == "dom_geometry_source_detached" {
                        stale("native geometry source detached or changed document")
                    } else { unavailable(reason) }
                })?;
                if css != after {
                    return Err(stale("CSS/native geometry source changed during measurement"));
                }
                let mapped = map_layout_bounds(&css.bounds(), css.viewport(), &first)
                    .map_err(unavailable)?;
                sample = DomNodeSample::NativeGeometry(json!({
                    "format":"native_dom_geometry_v1","status":"ok","mode":"native_element_geometry",
                    "ref":reference,"source_ref":reference,"target_id":target_id,"tab_id":tab_id,
                    "source":{"frame_id":identity.frame_id,"loader_id":identity.loader_id,
                        "backend_node_id":entry.backend_node_id,"context_unique_id":unique_id,
                        "generation":validated.record.generation},
                    "frame":{"kind":"main"},"css_sample":css.0,
                    "native":{"platform":"windows","pid":validated.record.pid,"window_id":validated.record.window_id,
                        "window":first.window,"viewport":first.viewport,"source":first.source,
                        "automation_id":"RootWebArea","framework":"Chrome","opaque_runtime_id":first.identity},
                    "physical_enclosing_rect":mapped.screen,"clipped":mapped.clipped,
                    "coordinate_space":"screen_physical_pixels","browser_version":browser_version,
                    "projection":"root_origin_plus_floor_start_ceil_end_at_sampled_dpr",
                    "qualification":"candidate_not_raster_qualified",
                    "atomic":false,"input_authority":false
                }));
            }
            let current = self
                .revalidate_for_mutation(session, target_id, Some(tab_id))
                .await?;
            if initial_cdp_window.is_some_and(|bounds| !current.native.geometry_exact || bounds != current.native.bounds) {
                return Err(stale("native CDP-correlation window geometry changed after viewport measurement"));
            }
            // Revalidation may create a new flattened attachment to the same
            // target. Its session label is not the document/source identity.
            if !Arc::ptr_eq(&current.conn, &validated.conn)
                || current.tab.cdp_target_id != validated.tab.cdp_target_id
                || current.record.generation != validated.record.generation
                || current.record.pid != validated.record.pid
                || current.record.window_id != validated.record.window_id
            {
                return Err(stale("geometry source connection changed"));
            }
            let retained = self
                .store
                .resolve_dom_ref(session, target_id, tab_id, reference)?;
            if retained.backend_node_id != entry.backend_node_id
                || retained.frame.kind != FrameKind::Main
                || retained.frame.identity != entry.frame.identity
            {
                return Err(stale("geometry inspection reference changed"));
            }
            let current_session = self
                .frame_session_for_mutation(session, target_id, tab_id, &current, &retained.frame)
                .await?;
            describe_source(&current.conn, &current_session, entry.backend_node_id).await?;
            self.require_same_selected_dom_target(&current).await?;
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            Ok(sample)
        })
        .await
        .unwrap_or_else(|_| Err(unavailable("bounded readonly geometry sample timed out")));
        // Cleanup has its own deadline. The guard also requests it if the caller
        // drops this future, including while this explicit cleanup is pending.
        let released = objects.release().await;
        drop(events);
        let result = match sampled {
            Err(error) => Err(error),
            Ok(_) if released.is_err() => Err(unavailable(
                "geometry remote-object cleanup could not be confirmed",
            )),
            Ok(sample) => Ok(sample),
        };
        if result
            .as_ref()
            .is_err_and(|error| matches!(error.code, BrowserRefusalCode::BrowserRefStale))
        {
            self.store
                .invalidate_tab_snapshots(session, target_id, tab_id);
        }
        result
    }
}

/// One native hit source, resolved only inside this request's attested document/world.
struct AttestedHit<'a> {
    conn: &'a CdpConnection,
    session: &'a str,
    frame_id: &'a str,
    context_id: i64,
    unique_id: &'a str,
    group: &'a str,
    object_id: &'a str,
}
impl AttestedHit<'_> {
    /// Shared by inspection and every dispatch step in that step's retained world.
    /// Never import pointer readiness from the JavaScript sample itself.
    async fn qualify_pointer(
        &self,
        reply: &Value,
        state: &mut ActionState,
        evidence: &mut ContextEvidence,
        events: &mut BoundedCdpEvents,
    ) -> Result<(), BrowserRefusal> {
        if reply.pointer("/result/value/receives_pointer") != Some(&Value::Null) {
            return Err(unavailable("dom_action_hit_unavailable"));
        }
        state.receives_pointer = match dom_action_hit::probe(reply).map_err(unavailable)? {
            HitProbe::Outside => Some(false),
            HitProbe::Unknown => None,
            HitProbe::Point { x, y } => {
                let first = self.sample(x, y).await?;
                evidence.drain(events, self.session, self.frame_id)?;
                self.confirm_sample(reply, &state.target).await?;
                evidence.drain(events, self.session, self.frame_id)?;
                let second = self.sample(x, y).await?;
                evidence.drain(events, self.session, self.frame_id)?;
                if first != second {
                    return Err(unavailable("dom_action_hit_changed"));
                }
                self.confirm_sample(reply, &state.target).await?;
                first.1
            }
        };
        evidence.drain(events, self.session, self.frame_id)
    }

    async fn confirm_sample(
        &self,
        expected: &Value,
        target: &ActionTarget,
    ) -> Result<(), BrowserRefusal> {
        let reply = read(self.conn, self.session, "Runtime.callFunctionOn", json!({
            "functionDeclaration":dom_action_sample::READ_FUNCTION,"uniqueContextId":self.unique_id,
            "arguments":[{"objectId":self.object_id}],"returnByValue":true,
            "awaitPromise":false,"userGesture":false,"throwOnSideEffect":true
        })).await?;
        DomNodeSampleKind::Actionability.parse(&reply, target.clone())?;
        if reply.pointer("/result/value") != expected.pointer("/result/value") {
            return Err(unavailable("dom_action_sample_changed"));
        }
        Ok(())
    }
    async fn sample(&self, x: i64, y: i64) -> Result<(i64, Option<bool>), BrowserRefusal> {
        // Chromium's inspector multiplies document-CSS x/y by LayoutZoomFactor,
        // then DocumentToFrame subtracts layout scroll. This is not a screen point.
        // UA internals explicitly retarget to their author host; authored closed
        // shadow roots are NOT retargeted and remain unknown in HIT_FUNCTION.
        let native = read(
            self.conn,
            self.session,
            "DOM.getNodeForLocation",
            json!({
                "x":x,"y":y,"includeUserAgentShadowDOM":false,"ignorePointerEventsNone":false
            }),
        )
        .await?;
        let backend = dom_action_hit::hit_identity(&native, self.frame_id).map_err(unavailable)?;
        let description = read_node(
            self.conn,
            self.session,
            "DOM.describeNode",
            json!({
                "backendNodeId":backend,"depth":0,"pierce":false
            }),
        )
        .await?;
        let node = description
            .get("node")
            .ok_or_else(|| unavailable("dom_action_hit_unavailable"))?;
        if node.get("backendNodeId").and_then(Value::as_i64) != Some(backend)
            || node.get("nodeType").and_then(Value::as_i64) != Some(1)
            || node.get("shadowRootType").is_some()
            || node.get("pseudoType").is_some()
            || node.get("contentDocument").is_some()
            || matches!(
                node.get("nodeName").and_then(Value::as_str),
                Some("IFRAME" | "FRAME" | "OBJECT" | "EMBED")
            )
        {
            return Err(unavailable("dom_action_hit_unavailable"));
        }
        let resolved = read_node(self.conn, self.session, "DOM.resolveNode", json!({
            "backendNodeId":backend,"executionContextId":self.context_id,"objectGroup":self.group
        })).await?;
        if resolved.pointer("/object/type").and_then(Value::as_str) != Some("object")
            || resolved.pointer("/object/subtype").and_then(Value::as_str) != Some("node")
        {
            return Err(stale("dom_action_ref_stale"));
        }
        let hit_object = resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .ok_or_else(|| stale("dom_action_ref_stale"))?;
        let reply = read(self.conn, self.session, "Runtime.callFunctionOn", json!({
            "functionDeclaration":dom_action_hit::HIT_FUNCTION,"uniqueContextId":self.unique_id,
            "arguments":[{"objectId":self.object_id},{"objectId":hit_object}],
            "returnByValue":true,"awaitPromise":false,"userGesture":false,"throwOnSideEffect":true
        })).await?;
        let relation = dom_action_hit::relation(&reply).map_err(|reason| {
            if reason == "dom_action_ref_stale" {
                stale(reason)
            } else {
                unavailable(reason)
            }
        })?;
        Ok((backend, relation))
    }
}
/// Owns only this request's remote handles. Cancellation cannot skip release.
struct RemoteObjectGroup {
    conn: Arc<CdpConnection>,
    session: String,
    name: String,
    armed: bool,
}

impl RemoteObjectGroup {
    async fn release(&mut self) -> Result<(), ()> {
        let released = release_group(&self.conn, &self.session, &self.name).await;
        self.armed = false;
        released
    }
}

impl Drop for RemoteObjectGroup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let conn = self.conn.clone();
        let session = self.session.clone();
        let group = self.name.clone();
        // The CDP connection requires Tokio too. If its runtime is already gone,
        // connection/session teardown is the remaining owner of remote cleanup.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = release_group(&conn, &session, &group).await;
            });
        }
    }
}

async fn release_group(conn: &CdpConnection, session: &str, group: &str) -> Result<(), ()> {
    match tokio::time::timeout(
        Duration::from_secs(2),
        read(
            conn,
            session,
            "Runtime.releaseObjectGroup",
            json!({"objectGroup":group}),
        ),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        _ => Err(()),
    }
}
fn unavailable(message: &str) -> BrowserRefusal {
    refuse(BrowserRefusalCode::BrowserRouteUnavailable, message)
}

fn stale(message: &str) -> BrowserRefusal {
    refuse(BrowserRefusalCode::BrowserRefStale, message)
}

async fn read(
    conn: &CdpConnection,
    session: &str,
    method: &str,
    args: Value,
) -> Result<Value, BrowserRefusal> {
    let reply = conn
        .call_bounded(Some(session), method, args, REPLY_LIMIT)
        .await
        .map_err(|_| unavailable("bounded isolated geometry protocol read is unavailable"))?;
    super::super::dom_read_diagnostic::check(method, &reply).map_err(unavailable)?;
    Ok(reply)
}

async fn read_node(
    conn: &CdpConnection,
    session: &str,
    method: &str,
    args: Value,
) -> Result<Value, BrowserRefusal> {
    conn.call_bounded(Some(session), method, args, REPLY_LIMIT)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("Could not find node")
                || message.contains("No node with given id")
                || message.contains("Node with given id does not belong to the document")
            {
                stale("geometry source node is no longer available")
            } else {
                unavailable("bounded geometry node read is unavailable")
            }
        })
}

async fn describe_source(
    conn: &CdpConnection,
    session: &str,
    backend: i64,
) -> Result<(), BrowserRefusal> {
    let description = read_node(
        conn,
        session,
        "DOM.describeNode",
        json!({
            "backendNodeId":backend,"depth":0
        }),
    )
    .await?;
    if description
        .pointer("/node/backendNodeId")
        .and_then(Value::as_i64)
        != Some(backend)
    {
        return Err(stale("geometry source node identity is unavailable"));
    }
    Ok(())
}

struct ContextEvidence {
    id: i64,
    world_name: &'static str,
    unique_id: Option<String>,
    consumed: usize,
}

impl ContextEvidence {
    fn drain(
        &mut self,
        events: &mut BoundedCdpEvents,
        session: &str,
        frame: &str,
    ) -> Result<(), BrowserRefusal> {
        loop {
            let event = match events.try_recv() {
                Ok(event) => event,
                Err(BoundedEventError::Empty) => return Ok(()),
                Err(BoundedEventError::Overflow) => {
                    return Err(unavailable("geometry context evidence exceeded its bound"));
                }
                Err(BoundedEventError::Closed) => {
                    return Err(unavailable("geometry event source closed"))
                }
            };
            self.consumed += 1;
            if self.consumed > EVENT_LIMIT || event.params.to_string().len() > REPLY_LIMIT {
                return Err(unavailable("geometry context evidence exceeded its bound"));
            }
            if event.session_id.as_deref() != Some(session) {
                continue;
            }
            if event.method == "Runtime.executionContextsCleared"
                || (event.method == "Runtime.executionContextDestroyed"
                    && (event
                        .params
                        .get("executionContextId")
                        .and_then(Value::as_i64)
                        == Some(self.id)
                        || self.unique_id.as_deref().is_some_and(|unique| {
                            event
                                .params
                                .get("executionContextUniqueId")
                                .and_then(Value::as_str)
                                == Some(unique)
                        })))
            {
                return Err(stale("isolated geometry context was retired"));
            }
            if event.method != "Runtime.executionContextCreated" {
                continue;
            }
            let context = &event.params["context"];
            if context.get("id").and_then(Value::as_i64) != Some(self.id) {
                continue;
            }
            let unique = context
                .get("uniqueId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 512)
                .ok_or_else(|| unavailable("isolated context lacks a bounded unique identity"))?;
            if context.get("name").and_then(Value::as_str) != Some(self.world_name)
                || context.pointer("/auxData/frameId").and_then(Value::as_str) != Some(frame)
                || context
                    .pointer("/auxData/isDefault")
                    .and_then(Value::as_bool)
                    != Some(false)
                || self.unique_id.is_some()
            {
                return Err(unavailable(
                    "isolated geometry context evidence is ambiguous or mismatched",
                ));
            }
            self.unique_id = Some(unique.to_owned());
        }
    }
}
