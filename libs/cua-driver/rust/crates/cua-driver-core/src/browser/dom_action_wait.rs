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

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: i64) -> ActionTarget {
        ActionTarget {
            frame_id: "main".into(),
            loader_id: "loader".into(),
            backend_node_id: id,
        }
    }

    fn pointer() -> ActionRequirements {
        ActionRequirements {
            visible: true,
            enabled: true,
            editable: false,
            stable: true,
            hit_test: true,
        }
    }
    fn state() -> ActionState {
        ActionState {
            target: target(11),
            connected: true,
            visible: Some(true),
            enabled: Some(true),
            editable: Some(true),
            receives_pointer: Some(true),
            bounds: Some([10.25, 20.5, 60.0, 30.0]),
        }
    }
    fn waiter() -> DomActionWait {
        DomActionWait::new(Duration::from_secs(1), pointer()).unwrap()
    }
    fn poll(wait: &mut DomActionWait, ms: u64, rows: &[ActionState]) -> ActionReadiness {
        wait.observe(Duration::from_millis(ms), true, rows, false)
    }

    #[test]
    fn async_absence_can_be_waited_but_a_ready_node_needs_two_separated_stable_samples() {
        let mut wait = waiter();
        assert_eq!(
            poll(&mut wait, 0, &[]),
            ActionReadiness::Pending("not_found")
        );
        assert_eq!(
            poll(&mut wait, 10, &[state()]),
            ActionReadiness::Pending("stability")
        );
        assert_eq!(
            poll(&mut wait, 109, &[state()]),
            ActionReadiness::Pending("stability")
        );
        assert_eq!(
            poll(&mut wait, 110, &[state()]),
            ActionReadiness::Ready(target(11))
        );
        assert_eq!(
            poll(&mut wait, 111, &[state()]),
            ActionReadiness::Refused("dom_action_already_resolved")
        );
    }

    #[test]
    fn ambiguity_and_incomplete_traversal_never_choose_a_convenient_first_match() {
        let mut other = state();
        other.target = target(12);
        assert_eq!(
            poll(&mut waiter(), 0, &[state(), other]),
            ActionReadiness::Refused("dom_action_ambiguous")
        );
        for rows in [vec![], vec![state()]] {
            assert_eq!(
                waiter().observe(Duration::ZERO, false, &rows, false),
                ActionReadiness::Refused("dom_action_coverage_incomplete")
            );
        }
    }

    #[test]
    fn replacement_detachment_and_document_change_refuse_instead_of_retargeting() {
        for changed in [
            None,
            Some(target(12)),
            Some(ActionTarget {
                loader_id: "new-loader".into(),
                ..target(11)
            }),
            Some(ActionTarget {
                frame_id: "other-frame".into(),
                ..target(11)
            }),
        ] {
            let mut wait = waiter();
            assert_eq!(
                poll(&mut wait, 0, &[state()]),
                ActionReadiness::Pending("stability")
            );
            let rows = changed
                .map(|target| ActionState { target, ..state() })
                .into_iter()
                .collect::<Vec<_>>();
            assert_eq!(
                poll(&mut wait, 100, &rows),
                ActionReadiness::Refused("dom_action_ref_stale")
            );
        }
        let detached = ActionState {
            connected: false,
            ..state()
        };
        assert_eq!(
            poll(&mut waiter(), 0, &[detached]),
            ActionReadiness::Refused("dom_action_ref_stale")
        );
    }

    #[test]
    fn hidden_disabled_readonly_and_occluded_nodes_wait_without_becoming_ready() {
        for (row, reason) in [
            (
                ActionState {
                    visible: Some(false),
                    ..state()
                },
                "not_visible",
            ),
            (
                ActionState {
                    enabled: Some(false),
                    ..state()
                },
                "disabled",
            ),
            (
                ActionState {
                    receives_pointer: Some(false),
                    ..state()
                },
                "hit_test",
            ),
        ] {
            assert_eq!(
                poll(&mut waiter(), 0, &[row]),
                ActionReadiness::Pending(reason)
            );
        }
        let mut wait = DomActionWait::new(
            Duration::from_secs(1),
            ActionRequirements {
                editable: true,
                ..pointer()
            },
        )
        .unwrap();
        assert_eq!(
            poll(
                &mut wait,
                0,
                &[ActionState {
                    editable: Some(false),
                    ..state()
                }]
            ),
            ActionReadiness::Pending("not_editable")
        );
        assert_eq!(
            poll(&mut wait, 1000, &[state()]),
            ActionReadiness::Refused("dom_action_timeout")
        );
    }

    #[test]
    fn missing_or_malformed_facts_are_unavailable_not_zero_or_false() {
        for row in [
            ActionState {
                visible: None,
                ..state()
            },
            ActionState {
                enabled: None,
                ..state()
            },
            ActionState {
                receives_pointer: None,
                ..state()
            },
            ActionState {
                bounds: None,
                ..state()
            },
            ActionState {
                bounds: Some([f64::NAN, 0.0, 10.0, 10.0]),
                ..state()
            },
            ActionState {
                bounds: Some([0.0, 0.0, -1.0, 10.0]),
                ..state()
            },
        ] {
            assert_eq!(
                poll(&mut waiter(), 0, &[row]),
                ActionReadiness::Refused("dom_action_state_unavailable")
            );
        }
        assert_eq!(
            poll(
                &mut waiter(),
                0,
                &[ActionState {
                    target: target(0),
                    ..state()
                }]
            ),
            ActionReadiness::Refused("dom_action_state_unavailable")
        );
    }

    #[test]
    fn layout_changes_reset_the_stability_interval_without_tolerance_or_scale_conversion() {
        let mut wait = waiter();
        assert_eq!(
            poll(&mut wait, 0, &[state()]),
            ActionReadiness::Pending("stability")
        );
        let moved = ActionState {
            bounds: Some([10.2500000001, 20.5, 60.0, 30.0]),
            ..state()
        };
        assert_eq!(
            poll(&mut wait, 100, &[moved.clone()]),
            ActionReadiness::Pending("stability")
        );
        assert_eq!(
            poll(&mut wait, 199, &[moved.clone()]),
            ActionReadiness::Pending("stability")
        );
        assert_eq!(
            poll(&mut wait, 200, &[moved]),
            ActionReadiness::Ready(target(11))
        );
    }

    #[test]
    fn cancellation_deadline_and_invalid_clock_are_terminal_before_any_readiness() {
        let mut wait = waiter();
        assert_eq!(
            wait.observe(Duration::ZERO, true, &[state()], true),
            ActionReadiness::Refused("dom_action_cancelled")
        );
        assert_eq!(
            poll(&mut wait, 100, &[state()]),
            ActionReadiness::Refused("dom_action_already_resolved")
        );
        assert_eq!(
            poll(&mut waiter(), 1000, &[state()]),
            ActionReadiness::Refused("dom_action_timeout")
        );
        let mut wait = waiter();
        poll(&mut wait, 100, &[state()]);
        assert_eq!(
            poll(&mut wait, 99, &[state()]),
            ActionReadiness::Refused("dom_action_clock_invalid")
        );
    }

    #[test]
    fn deadline_and_sample_budgets_are_bounded_even_for_a_stalled_or_equal_clock() {
        assert!(matches!(
            DomActionWait::new(Duration::ZERO, pointer()),
            Err("dom_action_invalid_timeout")
        ));
        assert!(matches!(
            DomActionWait::new(Duration::from_millis(10001), pointer()),
            Err("dom_action_invalid_timeout")
        ));
        let mut wait = waiter();
        for _ in 0..128 {
            assert_eq!(
                poll(&mut wait, 0, &[]),
                ActionReadiness::Pending("not_found")
            );
        }
        assert_eq!(
            poll(&mut wait, 0, &[]),
            ActionReadiness::Refused("dom_action_sample_limit")
        );
    }
}
