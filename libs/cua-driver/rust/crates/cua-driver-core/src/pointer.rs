//! One native pointer movement followed by bounded, interruptible dwell.
//! Activation and effect measurement belong to the native adapter, not this engine.

/// A precise interruption code and its native or timing detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Stable native or engine refusal/interruption code.
    pub code: &'static str,
    /// Detail for the caller; never interpreted as permission to retry.
    pub detail: String,
}

/// Native adapter seam. No method may retry motion or send button/key events.
pub trait Backend {
    /// Check cancellation, exact target, geometry and any live input interference.
    fn check_target(&mut self) -> Result<(), Failure>;
    /// Insert exactly one native movement. Dwell starts once delivery is confirmed.
    fn move_once(&mut self) -> Result<(), Failure>;
    /// Wait to the monotonic offset after movement, stopping on interference.
    fn wait_until(&mut self, elapsed_ms: u64) -> Result<(), Failure>;
    /// Monotonic dwell milliseconds since confirmed delivery, or zero before confirmation.
    fn elapsed_ms(&self) -> u64;
}

/// Retained execution prefix. Attempted input is never replayed after a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The native movement method was entered, even if its insertion failed.
    pub attempted: bool,
    /// The native movement method returned successful delivery.
    pub delivered: bool,
    /// Actual backend clock elapsed after motion, or zero before an attempt.
    pub dwell_elapsed_ms: u64,
    /// The first refusal or interruption; the delivered prefix remains intact.
    pub failure: Option<Failure>,
}

/// Move once, then wait with target checks at no more than ten-millisecond intervals.
/// A backend can interrupt sooner. Scheduling delay may extend actual elapsed time.
pub fn execute(dwell_ms: u64, backend: &mut impl Backend) -> Report {
    let mut report = Report {
        attempted: false,
        delivered: false,
        dwell_elapsed_ms: 0,
        failure: None,
    };
    let result = (|| {
        if dwell_ms > cua_driver_contract::POINTER_MAX_DWELL_MS {
            return Err(Failure {
                code: "invalid_arguments",
                detail: "move_pointer dwell_ms must be at most 10000".into(),
            });
        }
        backend.check_target()?;
        report.attempted = true;
        backend.move_once()?;
        report.delivered = true;
        loop {
            backend.check_target()?;
            let elapsed = backend.elapsed_ms();
            if elapsed >= dwell_ms {
                return Ok(());
            }
            backend.wait_until(elapsed.saturating_add(10).min(dwell_ms))?;
            if backend.elapsed_ms() <= elapsed {
                return Err(Failure {
                    code: "pointer_clock_stalled",
                    detail: "native pointer dwell clock did not advance".into(),
                });
            }
        }
    })();
    if report.attempted {
        report.dwell_elapsed_ms = backend.elapsed_ms();
    }
    report.failure = result.err();
    report
}
