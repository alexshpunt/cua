//! Readonly preparation of a locator action. No dispatch or action capability is minted here.

use super::super::dom_action_key::{parse_press_key, DomPressKey};
use super::super::dom_action_request::{DomActionKind, DomActionRequest};
use super::super::dom_action_wait::{
    ActionReadiness, ActionRequirements, ActionState, DomActionWait,
};
use super::super::types::ProcessFingerprint;
use super::*;
use std::time::{Duration, Instant};

/// One current readonly candidate. A mutator must separately admit the explicit action intent,
/// revalidate this source and actionability immediately before dispatch, and retain its prefix.
pub(crate) struct PreparedDomAction {
    pub(crate) reference: String,
    pub(crate) state: ActionState,
    pub(crate) press_key: Option<DomPressKey>,
}

#[derive(Clone, PartialEq, Eq)]
struct Source {
    fingerprint: ProcessFingerprint,
    window: u64,
    endpoint: String,
    target: String,
    generation: u64,
    tab_generation: u64,
    document: FrameIdentity,
}

fn unavailable(reason: &'static str) -> BrowserRefusal {
    refuse(BrowserRefusalCode::BrowserActionUnavailable, reason).with_detail(json!({
        "reason":reason,"phase":"readiness","mutation_attempted":false,"automatic_retry":false
    }))
}

pub(super) fn requirements(kind: DomActionKind) -> ActionRequirements {
    use DomActionKind::*;
    ActionRequirements {
        visible: true,
        enabled: !matches!(kind, Hover | ScrollIntoView),
        editable: matches!(kind, Fill | Type),
        stable: matches!(
            kind,
            Click | DoubleClick | RightClick | Hover | Check | Uncheck
        ),
        hit_test: matches!(
            kind,
            Click | DoubleClick | RightClick | Hover | Check | Uncheck
        ),
    }
}

impl BrowserEngine {
    /// Wait only within the initial inspection document; readonly refs remain readonly.
    /// The source reference supplies document context, never authorization to mutate its node.
    pub(crate) async fn prepare_dom_action(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        source_ref: &str,
        request: &DomActionRequest,
    ) -> Result<PreparedDomAction, BrowserRefusal> {
        let press_key = if request.kind == DomActionKind::Press {
            Some(parse_press_key(request.key.as_deref().unwrap_or("")).map_err(unavailable)?)
        } else {
            None
        };
        let timeout = Duration::from_millis(request.timeout_ms);
        let mut wait =
            DomActionWait::new(timeout, requirements(request.kind)).map_err(unavailable)?;
        let context = self
            .store
            .resolve_dom_ref(session, target_id, tab_id, source_ref)?;
        if context.frame.kind != FrameKind::Main || context.frame.oopif_target_id.is_some() {
            return Err(unavailable("dom_action_frame_unavailable"));
        }
        let identity = context
            .frame
            .identity
            .clone()
            .ok_or_else(|| unavailable("dom_action_source_unavailable"))?;
        let expected = self.action_source(session, target_id, tab_id, identity)?;
        let started = Instant::now();
        tokio::time::timeout(timeout, async {
            loop {
                let snapshot = self
                    .snapshot_tab_dom(
                        session,
                        target_id,
                        tab_id,
                        &request.locator,
                        None,
                        true,
                        200,
                        32,
                    )
                    .await?;
                let id = snapshot
                    .pointer("/snapshot/id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.strip_prefix('p'))
                    .and_then(|id| id.parse::<u64>().ok())
                    .ok_or_else(|| unavailable("dom_action_source_unavailable"))?;
                let record = self.store.get_target(session, target_id)?;
                let native_snapshot = record
                    .tabs
                    .get(tab_id)
                    .and_then(|tab| tab.snapshots.get(&id))
                    .ok_or_else(|| unavailable("dom_action_source_unavailable"))?;
                let document = native_snapshot
                    .semantic_root_identity
                    .clone()
                    .ok_or_else(|| unavailable("dom_action_source_unavailable"))?;
                if self.action_source(session, target_id, tab_id, document)? != expected {
                    return Err(unavailable("dom_action_source_changed"));
                }
                let rows = snapshot
                    .get("elements")
                    .and_then(Value::as_array)
                    .ok_or_else(|| unavailable("dom_action_state_unavailable"))?;
                // A known duplicate already proves ambiguity even when another branch is partial.
                if rows.len() > 1 {
                    return Err(unavailable("dom_action_ambiguous"));
                }
                let returned = snapshot
                    .pointer("/snapshot/returned_count")
                    .and_then(Value::as_u64);
                let complete = snapshot
                    .pointer("/snapshot/collection_complete")
                    .and_then(Value::as_bool)
                    == Some(true)
                    && snapshot
                        .pointer("/snapshot/truncated")
                        .and_then(Value::as_bool)
                        == Some(false)
                    && returned == Some(rows.len() as u64)
                    && snapshot
                        .pointer("/snapshot/matched_count")
                        .and_then(Value::as_u64)
                        == returned;
                if !complete {
                    return Err(unavailable("dom_action_coverage_incomplete"));
                }
                let mut sampled = Vec::new();
                let mut reference = None;
                if let Some(row) = rows.first() {
                    let current = row
                        .get("ref")
                        .and_then(Value::as_str)
                        .filter(|reference| reference.len() <= 256)
                        .ok_or_else(|| unavailable("dom_action_state_unavailable"))?;
                    let state = self
                        .inspect_dom_actionability(session, target_id, tab_id, current)
                        .await?;
                    if state.target.frame_id != expected.document.frame_id
                        || state.target.loader_id != expected.document.loader_id
                    {
                        return Err(unavailable("dom_action_source_changed"));
                    }
                    reference = Some(current.to_owned());
                    sampled.push(state);
                }
                if self.action_source(session, target_id, tab_id, expected.document.clone())?
                    != expected
                {
                    return Err(unavailable("dom_action_source_changed"));
                }
                match wait.observe(started.elapsed(), true, &sampled, false) {
                    ActionReadiness::Ready(_) => {
                        // The sampler already revalidated the current ref and source after its read.
                        // This value deliberately remains a readonly preparation, not a reusable permit.
                        return Ok(PreparedDomAction {
                            reference: reference.expect("readiness requires one sampled node"),
                            state: sampled.pop().expect("readiness requires one sampled node"),
                            press_key,
                        });
                    }
                    ActionReadiness::Refused(reason) => return Err(unavailable(reason)),
                    ActionReadiness::Pending(_) => {
                        tokio::time::sleep(Duration::from_millis(100)).await
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(unavailable("dom_action_timeout")))
    }

    fn action_source(
        &self,
        session: &str,
        target_id: &str,
        tab_id: &str,
        document: FrameIdentity,
    ) -> Result<Source, BrowserRefusal> {
        let record = self.store.get_target(session, target_id)?;
        let tab = record
            .tabs
            .get(tab_id)
            .ok_or_else(|| unavailable("dom_action_source_unavailable"))?;
        if document.frame_id.is_empty() || document.loader_id.is_empty() {
            return Err(unavailable("dom_action_source_unavailable"));
        }
        Ok(Source {
            fingerprint: record.fingerprint.clone(),
            window: record.window_id,
            endpoint: record.ws_url.clone(),
            target: tab.cdp_target_id.clone(),
            generation: record.generation,
            tab_generation: tab.generation,
            document,
        })
    }
}
