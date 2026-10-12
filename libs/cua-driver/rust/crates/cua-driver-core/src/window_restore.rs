//! One native restore request and explicit activation, with truthful interrupted prefixes.

pub use crate::pointer::Failure;

/// Maximum monotonic wait for observing restoration after one accepted request.
pub const RESTORE_MAX_WAIT_MS: u64 = 1_000;

/// Native seam. No method may inject input or replay restoration or activation.
pub trait Backend {
    /// Check cancellation and the admitted exact window lifetime.
    fn check_target(&mut self) -> Result<(), Failure>;
    /// Submit exactly one native restoration request; acceptance is not completion.
    fn request_restore(&mut self) -> Result<(), Failure>;
    /// Read whether the exact target is no longer minimized.
    fn is_restored(&mut self) -> Result<bool, Failure>;
    /// Attempt foreground activation once without keyboard or pointer events.
    fn activate_once(&mut self) -> Result<(), Failure>;
    /// Independently read whether this exact target is foreground.
    fn is_foreground(&mut self) -> Result<bool, Failure>;
    /// Monotonic milliseconds; wall-clock changes must not affect the wait.
    fn now_ms(&self) -> u64;
    /// Wait for at most the requested interval; the next check observes cancellation.
    fn wait_ms(&mut self, milliseconds: u64);
}

/// Exact retained execution prefix; an attempted request may already have effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The native restoration method was entered, even when acceptance is uncertain.
    pub restore_attempted: bool,
    /// The native backend accepted the single restoration request.
    pub restore_requested: bool,
    /// Restoration was observed before a later cancellation or failure.
    pub restored: bool,
    /// The non-input activation method was entered, even if it failed.
    pub activation_attempted: bool,
    /// The exact target was observed foreground after activation.
    pub activation_succeeded: bool,
    /// First interruption or refusal; prefixes never authorize replay.
    pub failure: Option<Failure>,
}

/// Restore once, observe completion within a bounded wait, then activate once.
/// Cancellation after an asynchronous request cannot retract its possible effect.
pub fn execute(backend: &mut impl Backend) -> Report {
    let mut report = Report {
        restore_attempted: false,
        restore_requested: false,
        restored: false,
        activation_attempted: false,
        activation_succeeded: false,
        failure: None,
    };
    let result = (|| {
        backend.check_target()?;
        report.restore_attempted = true;
        backend.request_restore()?;
        report.restore_requested = true;
        let started = backend.now_ms();
        loop {
            backend.check_target()?;
            if backend.is_restored()? {
                report.restored = true;
                break;
            }
            let now = backend.now_ms();
            let elapsed = now.checked_sub(started).ok_or_else(clock_stalled)?;
            if elapsed >= RESTORE_MAX_WAIT_MS {
                return Err(Failure {
                    code: "restore_timeout",
                    detail: "Restoration was not observed within the bounded wait".into(),
                });
            }
            backend.check_target()?;
            backend.wait_ms((RESTORE_MAX_WAIT_MS - elapsed).min(10));
            if backend.now_ms() <= now {
                return Err(clock_stalled());
            }
        }
        backend.check_target()?;
        report.activation_attempted = true;
        backend.activate_once()?;
        backend.check_target()?;
        if !backend.is_foreground()? {
            return Err(Failure {
                code: "activation_refused",
                detail: "The exact target was not observed foreground after activation".into(),
            });
        }
        report.activation_succeeded = true;
        backend.check_target()
    })();
    report.failure = result.err();
    report
}

fn clock_stalled() -> Failure {
    Failure {
        code: "restore_clock_stalled",
        detail: "The native monotonic clock did not advance during the restoration wait".into(),
    }
}
