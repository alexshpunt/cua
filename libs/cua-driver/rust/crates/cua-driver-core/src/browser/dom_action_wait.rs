//! Bounded locator readiness. Sampling never grants native-pixel or reusable input authority.

use std::time::Duration;

/// Exact node/document identity retained once a locator first resolves uniquely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionTarget {
    pub(crate) frame_id: String,
    pub(crate) loader_id: String,
    pub(crate) backend_node_id: i64,
}

/// Independently sampled actionability; missing facts are unknown, never implicit success.
#[derive(Debug, Clone)]
pub(crate) struct ActionState {
    pub(crate) target: ActionTarget,
    pub(crate) connected: bool,
    pub(crate) visible: Option<bool>,
    pub(crate) enabled: Option<bool>,
    pub(crate) editable: Option<bool>,
    pub(crate) receives_pointer: Option<bool>,
    pub(crate) bounds: Option<[f64; 4]>,
}

/// Required facts are chosen by the native action implementation, never by page content.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActionRequirements {
    pub(crate) visible: bool,
    pub(crate) enabled: bool,
    pub(crate) editable: bool,
    pub(crate) stable: bool,
    pub(crate) hit_test: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionReadiness {
    Pending(&'static str),
    Ready(ActionTarget),
    Refused(&'static str),
}

/// One request's finite wait. Re-resolving a locator cannot silently replace its pinned node.
/// Readiness is one-use and is not dispatch or proof of an application postcondition.
pub(crate) struct DomActionWait {
    timeout: Duration,
    requirements: ActionRequirements,
    pinned: Option<ActionTarget>,
    stable: Option<([f64; 4], Duration)>,
    last: Option<Duration>,
    samples: usize,
    resolved: bool,
}

impl DomActionWait {
    pub(crate) fn new(
        timeout: Duration,
        requirements: ActionRequirements,
    ) -> Result<Self, &'static str> {
        if timeout.is_zero() || timeout > Duration::from_secs(10) {
            return Err("dom_action_invalid_timeout");
        }
        Ok(Self {
            timeout,
            requirements,
            pinned: None,
            stable: None,
            last: None,
            samples: 0,
            resolved: false,
        })
    }

    fn refuse(&mut self, code: &'static str) -> ActionReadiness {
        self.resolved = true;
        ActionReadiness::Refused(code)
    }

    /// Observe one complete exact-locator result at a monotonic elapsed time.
    /// The caller must still revalidate source and cancellation immediately before dispatch.
    pub(crate) fn observe(
        &mut self,
        elapsed: Duration,
        complete: bool,
        candidates: &[ActionState],
        cancelled: bool,
    ) -> ActionReadiness {
        if self.resolved {
            return ActionReadiness::Refused("dom_action_already_resolved");
        }
        if cancelled {
            return self.refuse("dom_action_cancelled");
        }
        if elapsed >= self.timeout {
            return self.refuse("dom_action_timeout");
        }
        if self.last.is_some_and(|last| elapsed < last) {
            return self.refuse("dom_action_clock_invalid");
        }
        self.last = Some(elapsed);
        if self.samples >= 128 {
            return self.refuse("dom_action_sample_limit");
        }
        self.samples += 1;
        if !complete {
            return self.refuse("dom_action_coverage_incomplete");
        }
        if candidates.len() > 1 {
            return self.refuse("dom_action_ambiguous");
        }
        let Some(row) = candidates.first() else {
            return if self.pinned.is_some() {
                self.refuse("dom_action_ref_stale")
            } else {
                ActionReadiness::Pending("not_found")
            };
        };
        let target = &row.target;
        if target.backend_node_id <= 0
            || target.frame_id.is_empty()
            || target.loader_id.is_empty()
            || target.frame_id.len() > 256
            || target.loader_id.len() > 256
            || target.frame_id.contains('\0')
            || target.loader_id.contains('\0')
        {
            return self.refuse("dom_action_state_unavailable");
        }
        if !row.connected || self.pinned.as_ref().is_some_and(|pinned| pinned != target) {
            return self.refuse("dom_action_ref_stale");
        }
        self.pinned = Some(target.clone());
        let requirements = self.requirements;
        for (required, fact, pending) in [
            (requirements.visible, row.visible, "not_visible"),
            (requirements.enabled, row.enabled, "disabled"),
            (requirements.editable, row.editable, "not_editable"),
            (requirements.hit_test, row.receives_pointer, "hit_test"),
        ] {
            if !required {
                continue;
            }
            match fact {
                None => return self.refuse("dom_action_state_unavailable"),
                Some(false) => {
                    self.stable = None;
                    return ActionReadiness::Pending(pending);
                }
                Some(true) => {}
            }
        }
        if requirements.stable {
            let Some(bounds) = row.bounds.filter(|bounds| {
                bounds
                    .iter()
                    .all(|n| n.is_finite() && n.abs() <= 1_000_000_000.0)
                    && bounds[2] >= 0.0
                    && bounds[3] >= 0.0
                    && (bounds[0] + bounds[2]).abs() <= 1_000_000_000.0
                    && (bounds[1] + bounds[3]).abs() <= 1_000_000_000.0
            }) else {
                return self.refuse("dom_action_state_unavailable");
            };
            if bounds[2] == 0.0 || bounds[3] == 0.0 {
                self.stable = None;
                return ActionReadiness::Pending("no_layout_box");
            }
            match self.stable {
                Some((previous, since)) if previous == bounds => {
                    if elapsed - since < Duration::from_millis(100) {
                        return ActionReadiness::Pending("stability");
                    }
                }
                _ => {
                    self.stable = Some((bounds, elapsed));
                    return ActionReadiness::Pending("stability");
                }
            }
        }
        self.resolved = true;
        ActionReadiness::Ready(target.clone())
    }
}
