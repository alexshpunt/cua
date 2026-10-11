use cua_driver_core::pointer::{execute, Backend, Failure};

#[derive(Default)]
struct Journal {
    now: u64,
    moves: usize,
    waits: Vec<u64>,
    stop_at: Option<u64>,
    failed_move: bool,
    no_clock_progress: bool,
    cancel_wait_at: Option<u64>,
}
impl Backend for Journal {
    fn check_target(&mut self) -> Result<(), Failure> {
        if self.stop_at.is_some_and(|deadline| self.now >= deadline) {
            return Err(Failure {
                code: "target_lost",
                detail: "fixture target disappeared".into(),
            });
        }
        Ok(())
    }
    fn move_once(&mut self) -> Result<(), Failure> {
        self.moves += 1;
        self.now = 0;
        if self.failed_move {
            return Err(Failure {
                code: "input_failed",
                detail: "native insertion uncertain".into(),
            });
        }
        Ok(())
    }
    fn wait_until(&mut self, elapsed_ms: u64) -> Result<(), Failure> {
        self.waits.push(elapsed_ms);
        if let Some(cancel_at) = self.cancel_wait_at {
            self.now = cancel_at;
            return Err(Failure {
                code: "cancelled",
                detail: "fixture cancelled during native wait".into(),
            });
        }
        if !self.no_clock_progress {
            self.now = elapsed_ms;
        }
        Ok(())
    }
    fn elapsed_ms(&self) -> u64 {
        self.now
    }
}

#[test]
fn pointer_moves_once_and_checks_each_bounded_dwell_interval() {
    let mut native = Journal::default();
    let report = execute(25, &mut native);
    assert!(report.attempted && report.delivered);
    assert_eq!(report.failure, None);
    assert_eq!(report.dwell_elapsed_ms, 25);
    assert_eq!(native.moves, 1);
    assert_eq!(native.waits, [10, 20, 25]);
    let mut move_only = Journal::default();
    let report = execute(0, &mut move_only);
    assert!(report.delivered);
    assert_eq!(move_only.moves, 1);
    assert!(move_only.waits.is_empty());
}

#[test]
fn target_loss_before_and_after_motion_preserves_the_exact_prefix() {
    for (stop_at, moved, elapsed) in [(0, false, 0), (20, true, 20), (25, true, 25)] {
        let mut native = Journal {
            stop_at: Some(stop_at),
            ..Default::default()
        };
        let report = execute(25, &mut native);
        assert_eq!(report.attempted, moved);
        assert_eq!(report.delivered, moved);
        assert_eq!(report.dwell_elapsed_ms, elapsed);
        assert_eq!(report.failure.unwrap().code, "target_lost");
        assert_eq!(native.moves, usize::from(moved));
        assert_eq!(native.waits.last().copied().unwrap_or(0), elapsed);
    }
}

#[test]
fn uncertain_movement_is_never_replayed_or_followed_by_dwell() {
    let mut native = Journal {
        failed_move: true,
        ..Default::default()
    };
    let report = execute(25, &mut native);
    assert!(report.attempted);
    assert!(!report.delivered);
    assert_eq!(report.failure.unwrap().code, "input_failed");
    assert_eq!(native.moves, 1);
    assert!(native.waits.is_empty());
}

#[test]
fn a_broken_backend_clock_cannot_create_an_unbounded_loop() {
    let mut native = Journal {
        no_clock_progress: true,
        ..Default::default()
    };
    let report = execute(25, &mut native);
    assert!(report.attempted && report.delivered);
    assert_eq!(report.failure.unwrap().code, "pointer_clock_stalled");
    assert_eq!(native.moves, 1);
    assert_eq!(native.waits, [10]);
}

#[test]
fn dwell_cancellation_keeps_the_delivered_motion_and_elapsed_prefix() {
    let mut native = Journal {
        cancel_wait_at: Some(7),
        ..Default::default()
    };
    let report = execute(25, &mut native);
    assert!(report.attempted && report.delivered);
    assert_eq!(report.dwell_elapsed_ms, 7);
    assert_eq!(report.failure.unwrap().code, "cancelled");
    assert_eq!(native.moves, 1);
    assert_eq!(native.waits, [10]);
}

#[test]
fn excessive_dwell_refuses_before_native_motion() {
    let mut native = Journal::default();
    let report = execute(10001, &mut native);
    assert!(!report.attempted && !report.delivered);
    assert_eq!(report.failure.unwrap().code, "invalid_arguments");
    assert_eq!(native.moves, 0);
    assert!(native.waits.is_empty());
}
