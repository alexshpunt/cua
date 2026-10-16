//! Bounded, text-free evidence of native DOM action attempts and acknowledgements.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex, MutexGuard};

const INVALID: &str = "dom_action_progress_invalid";
const MAX_STEPS: usize = 32;

/// Shared request-local evidence. It never contains text, values or protocol identities.
#[derive(Clone, Default)]
pub(crate) struct DomActionProgress(Arc<Mutex<State>>);

struct State {
    status: &'static str,
    reason: Option<String>,
    attempted: Vec<&'static str>,
    acknowledged: Vec<&'static str>,
    in_flight: Option<&'static str>,
    cleanup: &'static str,
}

impl Default for State {
    fn default() -> Self {
        Self {
            status: "pending",
            reason: None,
            attempted: Vec::new(),
            acknowledged: Vec::new(),
            in_flight: None,
            cleanup: "not_started",
        }
    }
}

fn identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl DomActionProgress {
    fn state(&self) -> MutexGuard<'_, State> {
        // No user code runs under this lock. Preserve evidence even during unwinding.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record a bounded operation label before attempting its native call.
    pub(crate) fn begin_step(&self, name: &'static str) -> Result<(), &'static str> {
        let mut state = self.state();
        if state.status != "pending"
            || state.in_flight.is_some()
            || state.attempted.len() >= MAX_STEPS
            || !identifier(name, 64)
        {
            return Err(INVALID);
        }
        state.attempted.push(name);
        state.in_flight = Some(name);
        Ok(())
    }

    /// Record only a matching native acknowledgement, never an application-state claim.
    pub(crate) fn acknowledge_step(&self, name: &'static str) -> Result<(), &'static str> {
        let mut state = self.state();
        if state.status != "pending" || state.in_flight != Some(name) {
            return Err(INVALID);
        }
        state.acknowledged.push(name);
        state.in_flight = None;
        Ok(())
    }

    /// Seal the first terminal result without erasing an unknown in-flight effect.
    pub(crate) fn finish(&self, status: &'static str, reason: Option<&str>) {
        let mut state = self.state();
        if state.status != "pending" {
            return;
        }
        if !matches!(
            status,
            "dispatched" | "completed" | "refused" | "failed" | "cancelled" | "timeout" | "unknown"
        ) || reason.is_some_and(|reason| !identifier(reason, 96))
        {
            state.status = "failed";
            state.reason = Some(INVALID.to_owned());
        } else if matches!(status, "dispatched" | "completed") && state.in_flight.is_some() {
            state.status = "unknown";
            state.reason = Some("dom_action_unacknowledged_step".to_owned());
        } else {
            state.status = status;
            state.reason = reason.map(str::to_owned);
        }
    }

    /// Record owned cleanup independently of the action's already sealed outcome.
    pub(crate) fn cleanup(&self, confirmed: bool) {
        self.state().cleanup = if confirmed {
            "confirmed"
        } else {
            "unconfirmed"
        };
    }

    /// Return a detached bounded receipt; acknowledgements do not prove application effects.
    pub(crate) fn snapshot(&self) -> Value {
        let state = self.state();
        json!({
            "status":state.status,
            "reason":state.reason,
            "attempted_steps":state.attempted,
            "acknowledged_steps":state.acknowledged,
            "in_flight":state.in_flight,
            "mutation_attempted":!state.attempted.is_empty(),
            "effects":"unverified",
            "automatic_retry":false,
            "cleanup":state.cleanup
        })
    }
}

/// Marks cancellation if the containing future is dropped before its terminal receipt.
pub(crate) struct DomActionCancellation {
    progress: DomActionProgress,
    armed: bool,
}

impl DomActionCancellation {
    pub(crate) fn new(progress: DomActionProgress) -> Self {
        Self {
            progress,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DomActionCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.progress
                .finish("cancelled", Some("dom_action_cancelled_outcome_unknown"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unknown_step_keeps_the_acknowledged_prefix_without_claiming_application_success() {
        let progress = DomActionProgress::default();
        progress.begin_step("focus").unwrap();
        progress.acknowledge_step("focus").unwrap();
        progress.begin_step("input").unwrap();
        progress.finish("timeout", Some("dom_action_timeout"));
        progress.cleanup(false);
        let snapshot = progress.snapshot();
        assert_eq!(snapshot["attempted_steps"], json!(["focus", "input"]));
        assert_eq!(snapshot["acknowledged_steps"], json!(["focus"]));
        assert_eq!(snapshot["in_flight"], "input");
        assert_eq!(snapshot["mutation_attempted"], true);
        assert_eq!(snapshot["effects"], "unverified");
        assert_eq!(snapshot["automatic_retry"], false);
        assert_eq!(snapshot["cleanup"], "unconfirmed");
    }

    #[test]
    fn cancellation_is_recorded_even_when_the_future_never_dispatches() {
        let progress = DomActionProgress::default();
        {
            let _guard = DomActionCancellation::new(progress.clone());
        }
        let snapshot = progress.snapshot();
        assert_eq!(snapshot["status"], "cancelled");
        assert_eq!(snapshot["reason"], "dom_action_cancelled_outcome_unknown");
        assert_eq!(snapshot["mutation_attempted"], false);
        assert_eq!(snapshot["attempted_steps"], json!([]));
        assert_eq!(snapshot["in_flight"], serde_json::Value::Null);
    }

    #[test]
    fn cancellation_keeps_an_unacknowledged_attempt_and_never_replays_it() {
        let progress = DomActionProgress::default();
        {
            let _guard = DomActionCancellation::new(progress.clone());
            progress.begin_step("keydown").unwrap();
        }
        assert_eq!(progress.snapshot()["in_flight"], "keydown");
        assert_eq!(progress.snapshot()["attempted_steps"], json!(["keydown"]));
        assert_eq!(progress.snapshot()["acknowledged_steps"], json!([]));
        assert_eq!(
            progress.begin_step("keydown"),
            Err("dom_action_progress_invalid")
        );
    }

    #[test]
    fn invalid_transitions_do_not_fabricate_or_erase_a_prefix() {
        let progress = DomActionProgress::default();
        assert_eq!(
            progress.acknowledge_step("click"),
            Err("dom_action_progress_invalid")
        );
        progress.begin_step("click").unwrap();
        assert_eq!(
            progress.begin_step("focus"),
            Err("dom_action_progress_invalid")
        );
        assert_eq!(
            progress.acknowledge_step("focus"),
            Err("dom_action_progress_invalid")
        );
        assert_eq!(progress.snapshot()["in_flight"], "click");
        progress.acknowledge_step("click").unwrap();
        assert_eq!(
            progress.acknowledge_step("click"),
            Err("dom_action_progress_invalid")
        );
        progress.finish("dispatched", None);
        progress.finish("cancelled", Some("dom_action_cancelled_outcome_unknown"));
        progress.cleanup(true);
        assert_eq!(progress.snapshot()["status"], "dispatched");
        assert_eq!(progress.snapshot()["reason"], serde_json::Value::Null);
        assert_eq!(progress.snapshot()["acknowledged_steps"], json!(["click"]));
        assert_eq!(progress.snapshot()["cleanup"], "confirmed");
        assert_eq!(
            progress.begin_step("click"),
            Err("dom_action_progress_invalid")
        );
    }

    #[test]
    fn step_history_and_caller_metadata_stay_bounded() {
        let progress = DomActionProgress::default();
        assert_eq!(
            progress.begin_step("password=secret"),
            Err("dom_action_progress_invalid")
        );
        for _ in 0..32 {
            progress.begin_step("input").unwrap();
            progress.acknowledge_step("input").unwrap();
        }
        assert_eq!(
            progress.begin_step("input"),
            Err("dom_action_progress_invalid")
        );
        progress.finish("failed", Some("password=secret"));
        let snapshot = progress.snapshot();
        assert_eq!(snapshot["attempted_steps"].as_array().unwrap().len(), 32);
        assert_eq!(snapshot["reason"], "dom_action_progress_invalid");
        assert!(!snapshot.to_string().contains("secret"));
        assert_eq!(snapshot.as_object().unwrap().len(), 9);
    }

    #[test]
    fn an_unacknowledged_attempt_cannot_be_sealed_as_dispatched() {
        let progress = DomActionProgress::default();
        progress.begin_step("click").unwrap();
        progress.finish("dispatched", None);
        let mut receipt = progress.snapshot();
        assert_eq!(receipt["status"], "unknown");
        assert_eq!(receipt["reason"], "dom_action_unacknowledged_step");
        assert_eq!(receipt["in_flight"], "click");
        receipt["attempted_steps"] = json!([]);
        assert_eq!(progress.snapshot()["attempted_steps"], json!(["click"]));
    }

    #[test]
    fn a_disarmed_guard_does_not_replace_a_confirmed_terminal_result() {
        let progress = DomActionProgress::default();
        assert_eq!(progress.snapshot()["status"], "pending");
        assert_eq!(progress.snapshot()["cleanup"], "not_started");
        {
            let mut guard = DomActionCancellation::new(progress.clone());
            progress.finish("refused", Some("dom_action_route_unavailable"));
            guard.disarm();
        }
        assert_eq!(progress.snapshot()["status"], "refused");
        assert_eq!(
            progress.snapshot()["reason"],
            "dom_action_route_unavailable"
        );
    }
}
