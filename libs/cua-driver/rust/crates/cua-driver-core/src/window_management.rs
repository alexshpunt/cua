//! One native window-management request, bounded observation and truthful interruption prefixes.

pub use crate::pointer::Failure;
use cua_driver_contract::{
    DesktopWindowBounds, ToolInput, WindowControlSample, WindowManagementInput,
    WindowManagementOperation,
};

/// Maximum monotonic wait for one accepted native mutation's measured postcondition.
pub const WINDOW_MANAGEMENT_MAX_WAIT_MS: u64 = 1_000;

/// Native seam; implementations must not inject input, switch desktops or replay requests.
pub trait Backend {
    /// Monotonic milliseconds, independent of wall-clock adjustments.
    fn now_ms(&self) -> u64;
    /// Check cancellation and the admitted exact window lifetime.
    fn check_target(&mut self) -> Result<(), Failure>;
    /// Read independent metadata without renewing admission or capturing pixels.
    fn sample(&mut self) -> Result<WindowControlSample, Failure>;
    /// Submit exactly one operation; acceptance does not establish completion.
    fn request(
        &mut self,
        operation: WindowManagementOperation,
        requested_bounds: Option<DesktopWindowBounds>,
    ) -> Result<(), Failure>;
    /// Wait at most the requested interval, propagating cancellation or wait failure.
    fn wait_ms(&mut self, milliseconds: u64) -> Result<(), Failure>;
}

/// Retained native execution prefix. An attempted asynchronous request may already have effects.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub requested_bounds: Option<DesktopWindowBounds>,
    pub before: Option<WindowControlSample>,
    pub observed: Option<WindowControlSample>,
    pub mutation_attempted: bool,
    pub mutation_accepted: bool,
    pub postcondition_confirmed: bool,
    /// First failure overrides completed flags when deciding whether execution succeeded.
    pub failure: Option<Failure>,
}

/// Validate before touching the backend, request once and retain all measured prefixes.
/// Cancellation cannot retract an asynchronous request; a failed report never permits replay.
pub fn execute(input: &WindowManagementInput, backend: &mut impl Backend) -> Report {
    let mut report = Report {
        requested_bounds: None,
        before: None,
        observed: None,
        mutation_attempted: false,
        mutation_accepted: false,
        postcondition_confirmed: false,
        failure: None,
    };
    let result = (|| {
        input.validate().map_err(|detail| Failure {
            code: "invalid_arguments",
            detail,
        })?;
        backend.check_target()?;
        let before = backend.sample()?;
        validate_sample(&before, input.window_id)?;
        report.before = Some(before.clone());
        backend.check_target()?;
        check_preconditions(input.operation, &before)?;
        report.requested_bounds = match input.operation {
            WindowManagementOperation::Move => Some(DesktopWindowBounds {
                x: input.x.expect("validated move x"),
                y: input.y.expect("validated move y"),
                ..before.bounds
            }),
            WindowManagementOperation::Resize => Some(DesktopWindowBounds {
                width: input.width.expect("validated resize width"),
                height: input.height.expect("validated resize height"),
                ..before.bounds
            }),
            _ => None,
        };
        if let Some(bounds) = report.requested_bounds {
            bounds.validate().map_err(|detail| Failure {
                code: "invalid_arguments",
                detail,
            })?;
        }
        backend.check_target()?;
        report.mutation_attempted = true;
        backend.request(input.operation, report.requested_bounds)?;
        report.mutation_accepted = true;
        let started = backend.now_ms();
        loop {
            backend.check_target()?;
            let observed = backend.sample()?;
            validate_sample(&observed, input.window_id)?;
            let confirmed =
                postcondition(input.operation, report.requested_bounds, &before, &observed);
            report.observed = Some(observed);
            report.postcondition_confirmed = confirmed;
            backend.check_target()?;
            if confirmed {
                // A final cancellation may override even an entirely completed prefix.
                return backend.check_target();
            }
            let now = backend.now_ms();
            let elapsed = now.checked_sub(started).ok_or_else(clock_stalled)?;
            if elapsed >= WINDOW_MANAGEMENT_MAX_WAIT_MS {
                return Err(Failure {
                    code: "window_management_timeout",
                    detail:
                        "Requested window postcondition was not observed within the bounded wait"
                            .into(),
                });
            }
            backend.check_target()?;
            backend.wait_ms((WINDOW_MANAGEMENT_MAX_WAIT_MS - elapsed).min(10))?;
            if backend.now_ms() <= now {
                return Err(clock_stalled());
            }
        }
    })();
    report.failure = result.err();
    report
}

fn validate_sample(sample: &WindowControlSample, window_id: u64) -> Result<(), Failure> {
    sample.validate().map_err(|detail| Failure {
        code: "invalid_window_state",
        detail,
    })?;
    if sample.foreground_at_target != (sample.foreground_window == window_id) {
        return Err(Failure {
            code: "invalid_window_state",
            detail: "Foreground metadata does not match the exact window source".into(),
        });
    }
    Ok(())
}

fn check_preconditions(
    operation: WindowManagementOperation,
    before: &WindowControlSample,
) -> Result<(), Failure> {
    use WindowManagementOperation::*;
    let refusal = match operation {
        Move | Resize if before.minimized || before.maximized => Some((
            "window_not_normal",
            "Move and resize require a normal, non-maximized window",
        )),
        Maximize | Focus if before.minimized => Some((
            "window_minimized",
            "Use the separate restore operation before maximize or focus",
        )),
        Minimize if before.minimized => Some((
            "window_already_minimized",
            "The exact window is already minimized",
        )),
        Maximize if before.maximized => Some((
            "window_already_maximized",
            "The exact window is already maximized",
        )),
        _ => None,
    };
    if let Some((code, detail)) = refusal {
        return Err(Failure {
            code,
            detail: detail.into(),
        });
    }
    if matches!(operation, Maximize | Focus) {
        match before.virtual_desktop.on_current_desktop {
            Some(true) => {}
            Some(false) => return Err(Failure {
                code: "window_not_on_current_desktop",
                detail:
                    "The exact window is not on the current virtual desktop; no switch is attempted"
                        .into(),
            }),
            None => return Err(Failure {
                code: "window_desktop_unknown",
                detail:
                    "Current virtual-desktop membership is unavailable; no activation is attempted"
                        .into(),
            }),
        }
    }
    Ok(())
}

fn postcondition(
    operation: WindowManagementOperation,
    requested: Option<DesktopWindowBounds>,
    before: &WindowControlSample,
    observed: &WindowControlSample,
) -> bool {
    match operation {
        WindowManagementOperation::Move | WindowManagementOperation::Resize => {
            !observed.minimized
                && !observed.maximized
                && before.bounds_source == observed.bounds_source
                && requested == Some(observed.bounds)
        }
        WindowManagementOperation::Minimize => observed.minimized,
        WindowManagementOperation::Maximize => observed.maximized && !observed.minimized,
        WindowManagementOperation::Focus => observed.foreground_at_target && !observed.minimized,
    }
}

fn clock_stalled() -> Failure {
    Failure {
        code: "window_management_clock_stalled",
        detail: "The native monotonic clock did not advance during the window-management wait"
            .into(),
    }
}
