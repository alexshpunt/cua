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
