//! One native movement followed by checked, bounded wheel axes.
//! Activation, arrival confirmation and effect measurement belong to the adapter.

use crate::pointer::Failure;
use cua_driver_contract::{ScrollAxis, ScrollUnit};

/// Native seam; methods must never retry input or synthesize buttons/keys.
pub trait Backend {
    /// Check cancellation, exact target, geometry and live input interference.
    fn check_target(&mut self) -> Result<(), Failure>;
    /// Insert the one native movement; the next check confirms arrival.
    fn move_once(&mut self) -> Result<(), Failure>;
    /// Insert one wheel event for this axis using signed integer detents.
    fn scroll_once(&mut self, axis: ScrollAxis, detents: i32) -> Result<(), Failure>;
}

/// Exact retained native prefix, including uncertain attempted insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The native movement method was entered.
    pub pointer_attempted: bool,
    /// The native movement method returned successful delivery.
    pub pointer_delivered: bool,
    /// Each native wheel method entered, including the first uncertain one.
    pub attempted_axes: Vec<ScrollAxis>,
    /// Wheel methods that returned successful delivery.
    pub delivered_axes: Vec<ScrollAxis>,
    /// First refusal or interruption; never authorizes replay.
    pub failure: Option<Failure>,
}

/// Move once, then deliver nonzero vertical and horizontal axes in that order.
/// Check before and after each insertion and stop at the first interruption.
pub fn execute(delta_x: i32, delta_y: i32, backend: &mut impl Backend) -> Report {
    let mut report = Report {
        pointer_attempted: false,
        pointer_delivered: false,
        attempted_axes: vec![],
        delivered_axes: vec![],
        failure: None,
    };
    let result = (|| {
        ScrollUnit::WheelDetents
            .validate_deltas(delta_x, delta_y)
            .map_err(|detail| Failure {
                code: "invalid_arguments",
                detail,
            })?;
        backend.check_target()?;
        report.pointer_attempted = true;
        backend.move_once()?;
        report.pointer_delivered = true;
        backend.check_target()?;
        for (axis, detents) in [(ScrollAxis::Y, delta_y), (ScrollAxis::X, delta_x)] {
            if detents == 0 {
                continue;
            }
            backend.check_target()?;
            report.attempted_axes.push(axis);
            backend.scroll_once(axis, detents)?;
            report.delivered_axes.push(axis);
            backend.check_target()?;
        }
        Ok(())
    })();
    report.failure = result.err();
    report
}
