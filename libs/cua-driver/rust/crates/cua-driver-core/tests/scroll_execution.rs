use cua_driver_contract::ScrollAxis;
use cua_driver_core::pointer::Failure;
use cua_driver_core::scroll::{execute, Backend};

#[derive(Debug, PartialEq)]
enum Event {
    Check,
    Move,
    Wheel(ScrollAxis, i32),
}

#[derive(Default)]
struct Journal {
    events: Vec<Event>,
    checks: usize,
    stop_on_check: Option<usize>,
    fail_move: bool,
    fail_axis: Option<ScrollAxis>,
}

fn failure(code: &'static str) -> Failure {
    Failure {
        code,
        detail: "fixture interruption".into(),
    }
}

impl Backend for Journal {
    fn check_target(&mut self) -> Result<(), Failure> {
        self.events.push(Event::Check);
        self.checks += 1;
        if self.stop_on_check == Some(self.checks) {
            return Err(failure("cancelled"));
        }
        Ok(())
    }

    fn move_once(&mut self) -> Result<(), Failure> {
        self.events.push(Event::Move);
        if self.fail_move {
            return Err(failure("pointer_input_failed"));
        }
        Ok(())
    }

    fn scroll_once(&mut self, axis: ScrollAxis, detents: i32) -> Result<(), Failure> {
        self.events.push(Event::Wheel(axis, detents));
        if self.fail_axis == Some(axis) {
            return Err(failure("wheel_input_failed"));
        }
        Ok(())
    }
}

#[test]
fn one_move_precedes_checked_vertical_then_horizontal_wheel_input() {
    let mut native = Journal::default();
    let report = execute(2, -3, &mut native);
    assert!(report.pointer_attempted && report.pointer_delivered);
    assert_eq!(report.attempted_axes, [ScrollAxis::Y, ScrollAxis::X]);
    assert_eq!(report.delivered_axes, report.attempted_axes);
    assert_eq!(report.failure, None);
    assert_eq!(
        native.events,
        [
            Event::Check,
            Event::Move,
            Event::Check,
            Event::Check,
            Event::Wheel(ScrollAxis::Y, -3),
            Event::Check,
            Event::Check,
            Event::Wheel(ScrollAxis::X, 2),
            Event::Check,
        ]
    );
}

#[test]
fn zero_axes_are_skipped_without_changing_sign_or_repeating_motion() {
    for (x, y, axis, delta) in [(0, 50, ScrollAxis::Y, 50), (-50, 0, ScrollAxis::X, -50)] {
        let mut native = Journal::default();
        let report = execute(x, y, &mut native);
        assert_eq!(report.failure, None);
        assert_eq!(report.attempted_axes, [axis]);
        assert_eq!(report.delivered_axes, [axis]);
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| **event == Event::Move)
                .count(),
            1
        );
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| matches!(event, Event::Wheel(_, _)))
                .collect::<Vec<_>>(),
            [&Event::Wheel(axis, delta)]
        );
    }
}

#[test]
fn invalid_deltas_refuse_before_touching_the_native_backend() {
    for (x, y, detail) in [
        (0, 0, "scroll_at requires a nonzero delta"),
        (
            51,
            0,
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
        (
            0,
            -51,
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
        (
            i32::MIN,
            1,
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
        (
            i32::MAX,
            0,
            "scroll_at deltas must be within -50..50 wheel detents",
        ),
    ] {
        let mut native = Journal::default();
        let report = execute(x, y, &mut native);
        assert!(!report.pointer_attempted && !report.pointer_delivered);
        assert!(report.attempted_axes.is_empty() && report.delivered_axes.is_empty());
        assert_eq!(
            report.failure,
            Some(Failure {
                code: "invalid_arguments",
                detail: detail.into()
            })
        );
        assert!(native.events.is_empty());
    }
}

#[test]
fn cancellation_keeps_the_exact_prefix_before_motion_between_axes_and_after_delivery() {
    for (check, moved, axes) in [
        (1, false, vec![]),
        (2, true, vec![]),
        (3, true, vec![]),
        (4, true, vec![ScrollAxis::Y]),
        (5, true, vec![ScrollAxis::Y]),
        (6, true, vec![ScrollAxis::Y, ScrollAxis::X]),
    ] {
        let mut native = Journal {
            stop_on_check: Some(check),
            ..Default::default()
        };
        let report = execute(2, -3, &mut native);
        assert_eq!(report.failure.unwrap().code, "cancelled");
        assert_eq!(report.pointer_attempted, moved);
        assert_eq!(report.pointer_delivered, moved);
        assert_eq!(report.attempted_axes, axes);
        assert_eq!(report.delivered_axes, axes);
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| **event == Event::Move)
                .count(),
            usize::from(moved)
        );
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| matches!(event, Event::Wheel(_, _)))
                .count(),
            axes.len()
        );
        assert_eq!(native.checks, check);
    }
}

#[test]
fn uncertain_native_insertion_is_not_replayed_or_followed_by_another_axis() {
    let mut motion = Journal {
        fail_move: true,
        ..Default::default()
    };
    let report = execute(2, -3, &mut motion);
    assert!(report.pointer_attempted && !report.pointer_delivered);
    assert_eq!(report.failure.unwrap().code, "pointer_input_failed");
    assert!(report.attempted_axes.is_empty() && report.delivered_axes.is_empty());
    assert_eq!(motion.events, [Event::Check, Event::Move]);
    for axis in [ScrollAxis::Y, ScrollAxis::X] {
        let mut native = Journal {
            fail_axis: Some(axis),
            ..Default::default()
        };
        let report = execute(2, -3, &mut native);
        assert!(report.pointer_delivered);
        assert_eq!(report.failure.unwrap().code, "wheel_input_failed");
        let prefix = if axis == ScrollAxis::Y {
            vec![ScrollAxis::Y]
        } else {
            vec![ScrollAxis::Y, ScrollAxis::X]
        };
        let delivered = if axis == ScrollAxis::Y {
            vec![]
        } else {
            vec![ScrollAxis::Y]
        };
        assert_eq!(report.attempted_axes, prefix);
        assert_eq!(report.delivered_axes, delivered);
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| **event == Event::Move)
                .count(),
            1
        );
        assert_eq!(
            native
                .events
                .iter()
                .filter(|event| matches!(event, Event::Wheel(_, _)))
                .count(),
            prefix.len()
        );
    }
}
