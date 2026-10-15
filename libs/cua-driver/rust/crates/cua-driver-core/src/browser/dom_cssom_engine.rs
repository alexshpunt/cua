//! Explicit readonly CSSOM geometry; never a CSS-to-native-pixel mapping.

use super::super::cdp_ws::CdpEvent;
use super::super::dom_cssom_sample::{parse_sample, READ_FUNCTION, WORLD_NAME};
use super::*;
use std::time::Duration;
use tokio::sync::mpsc::{error::TryRecvError, UnboundedReceiver};

const REPLY_LIMIT: usize = 16 * 1024;
const EVENT_LIMIT: usize = 512;

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
        let group = format!("cua-dom-geometry-{}", Uuid::new_v4());
        let mut events = validated.conn.subscribe();
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
                    "frameId":identity.frame_id,"worldName":WORLD_NAME,"grantUniveralAccess":false
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
                    "functionDeclaration":READ_FUNCTION,"uniqueContextId":unique_id,
                    "arguments":[{"objectId":object_id}],
                    "returnByValue":true,"awaitPromise":false,"userGesture":false,
                    "throwOnSideEffect":true
                }),
            )
            .await?;
            let sample = parse_sample(&reply).map_err(|reason| {
                if reason == "dom_geometry_source_detached" {
                    stale("geometry source detached or changed document")
                } else {
                    unavailable("fixed side-effect-checked geometry sample is unavailable")
                }
            })?;
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            let current = self
                .revalidate_for_mutation(session, target_id, Some(tab_id))
                .await?;
            if current.cdp_session != validated.cdp_session
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
            if current_session != frame_session {
                return Err(stale("geometry source session changed"));
            }
            describe_source(&current.conn, &current_session, entry.backend_node_id).await?;
            self.require_same_selected_dom_target(&current).await?;
            evidence.drain(&mut events, &frame_session, &identity.frame_id)?;
            Ok(sample)
        })
        .await
        .unwrap_or_else(|_| Err(unavailable("bounded readonly geometry sample timed out")));
        // The read may have been cancelled after resolveNode allocated an object.
        // Release its group independently of the read deadline and original result.
        let released = tokio::time::timeout(
            Duration::from_secs(2),
            read(
                &validated.conn,
                &validated.cdp_session,
                "Runtime.releaseObjectGroup",
                json!({"objectGroup":group}),
            ),
        )
        .await;
        drop(events);
        let result = match sampled {
            Err(error) => Err(error),
            Ok(_) if !matches!(released, Ok(Ok(_))) => Err(unavailable(
                "geometry remote-object cleanup could not be confirmed",
            )),
            Ok(sample) => {
                let mut output = json!({
                    "status":"ok","mode":"element_geometry","ref":reference,
                    "source_ref":reference,"target_id":target_id,"tab_id":tab_id,
                    "frame":{"kind":"main"},"coordinate_space":"frame_viewport_css_pixels",
                    "method":"isolated_cssom_border_box","native_pixel_mapping":"unavailable",
                    "atomic":false
                });
                // parse_sample already projects this closed set; never expose raw CDP data.
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
    if !reply.is_object()
        || reply.get("exceptionDetails").is_some()
        || reply.get("wasThrown").and_then(Value::as_bool) == Some(true)
    {
        return Err(unavailable(
            "isolated geometry protocol read reported an exception or malformed reply",
        ));
    }
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
    unique_id: Option<String>,
    consumed: usize,
}

impl ContextEvidence {
    fn drain(
        &mut self,
        events: &mut UnboundedReceiver<CdpEvent>,
        session: &str,
        frame: &str,
    ) -> Result<(), BrowserRefusal> {
        loop {
            let event = match events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
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
            if context.get("name").and_then(Value::as_str) != Some(WORLD_NAME)
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
