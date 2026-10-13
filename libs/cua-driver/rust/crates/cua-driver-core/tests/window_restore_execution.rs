use cua_driver_core::window_restore::{execute, Backend, Failure, RESTORE_MAX_WAIT_MS};

#[derive(Default)]
struct Native {
    now: u64,
    restored_at: u64,
    cancelled: bool,
    cancel_after: Option<&'static str>,
    fail_at: Option<&'static str>,
    foreground: bool,
    stalled_clock: bool,
    journal: Vec<&'static str>,
    waits: Vec<u64>,
}
impl Native {
    fn event(&mut self, name: &'static str) -> Result<(), Failure> {
        self.journal.push(name);
        if self.cancel_after == Some(name) {
            self.cancelled = true;
        }
        if self.fail_at == Some(name) {
            Err(Failure {
                code: "native_uncertain",
                detail: format!("fixture {name} uncertain"),
            })
        } else {
            Ok(())
        }
    }
}
impl Backend for Native {
    fn check_target(&mut self) -> Result<(), Failure> {
        self.journal.push("check");
        if self.cancelled {
            Err(Failure {
                code: "cancelled",
                detail: "fixture cancelled".into(),
            })
        } else {
            Ok(())
        }
    }
    fn request_restore(&mut self) -> Result<(), Failure> {
        self.event("restore")
    }
    fn is_restored(&mut self) -> Result<bool, Failure> {
        self.event("restored")?;
        Ok(self.now >= self.restored_at)
    }
    fn activate_once(&mut self) -> Result<(), Failure> {
        self.event("activate")
    }
    fn is_foreground(&mut self) -> Result<bool, Failure> {
        self.event("foreground")?;
        Ok(self.foreground)
    }
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn wait_ms(&mut self, milliseconds: u64) {
        self.waits.push(milliseconds);
        if !self.stalled_clock {
            self.now += milliseconds;
        }
        if self.cancel_after == Some("wait") {
            self.cancelled = true;
        }
    }
}
fn target() -> Native {
    Native {
        foreground: true,
        ..Default::default()
    }
}

#[test]
fn one_restore_request_precedes_bounded_observation_and_one_verified_activation() {
    let mut native = Native {
        restored_at: 25,
        ..target()
    };
    let report = execute(&mut native);
    assert_eq!(report.failure, None);
    assert!(report.restore_attempted && report.restore_requested && report.restored);
    assert!(report.activation_attempted && report.activation_succeeded);
    assert_eq!(native.waits, [10, 10, 10]);
    assert_eq!(
        native
            .journal
            .iter()
            .filter(|name| **name == "restore")
            .count(),
        1
    );
    assert_eq!(
        native
            .journal
            .iter()
            .filter(|name| **name == "activate")
            .count(),
        1
    );
    assert_eq!(native.journal.last(), Some(&"check"));
    let activation = native
        .journal
        .iter()
        .position(|event| *event == "activate")
        .unwrap();
    assert_eq!(native.journal[activation - 1], "check");
}

#[test]
fn cancellation_before_dispatch_after_request_before_activation_and_after_confirmation_retains_prefixes(
) {
    for (cancelled, after, prefix) in [
        (true, None, [false, false, false, false, false]),
        (false, Some("restore"), [true, true, false, false, false]),
        (false, Some("restored"), [true, true, true, false, false]),
        (false, Some("activate"), [true, true, true, true, false]),
        (false, Some("foreground"), [true, true, true, true, true]),
    ] {
        let mut native = Native {
            cancelled,
            cancel_after: after,
            ..target()
        };
        let report = execute(&mut native);
        assert_eq!(report.failure.unwrap().code, "cancelled", "{after:?}");
        assert_eq!(
            [
                report.restore_attempted,
                report.restore_requested,
                report.restored,
                report.activation_attempted,
                report.activation_succeeded
            ],
            prefix,
            "{after:?}"
        );
        assert_eq!(
            native
                .journal
                .iter()
                .filter(|event| **event == "restore")
                .count(),
            usize::from(prefix[0])
        );
        assert_eq!(
            native
                .journal
                .iter()
                .filter(|event| **event == "activate")
                .count(),
            usize::from(prefix[3])
        );
        assert!(native.waits.is_empty());
    }
    let mut native = Native {
        restored_at: 100,
        cancel_after: Some("wait"),
        ..target()
    };
    let report = execute(&mut native);
    assert_eq!(report.failure.unwrap().code, "cancelled");
    assert!(report.restore_requested && !report.restored && !report.activation_attempted);
    assert_eq!(native.waits, [10]);
}

#[test]
fn uncertain_restore_or_activation_is_never_replayed_and_later_methods_stop() {
    for (fail_at, prefix) in [
        ("restore", [true, false, false, false, false]),
        ("restored", [true, true, false, false, false]),
        ("activate", [true, true, true, true, false]),
        ("foreground", [true, true, true, true, false]),
    ] {
        let mut native = Native {
            fail_at: Some(fail_at),
            ..target()
        };
        let report = execute(&mut native);
        assert_eq!(report.failure.unwrap().code, "native_uncertain");
        assert_eq!(
            [
                report.restore_attempted,
                report.restore_requested,
                report.restored,
                report.activation_attempted,
                report.activation_succeeded
            ],
            prefix,
            "{fail_at}"
        );
        assert_eq!(
            native
                .journal
                .iter()
                .filter(|event| **event == "restore")
                .count(),
            1
        );
        assert_eq!(
            native
                .journal
                .iter()
                .filter(|event| **event == "activate")
                .count(),
            usize::from(prefix[3])
        );
        assert!(native.waits.is_empty());
    }
}

#[test]
fn pending_restore_times_out_and_a_stalled_clock_cannot_make_an_unbounded_wait() {
    let mut pending = Native {
        restored_at: RESTORE_MAX_WAIT_MS + 1,
        ..target()
    };
    let report = execute(&mut pending);
    assert_eq!(report.failure.unwrap().code, "restore_timeout");
    assert!(report.restore_attempted && report.restore_requested);
    assert!(!report.restored && !report.activation_attempted);
    assert_eq!(pending.waits.iter().sum::<u64>(), RESTORE_MAX_WAIT_MS);
    assert!(pending.waits.iter().all(|milliseconds| *milliseconds <= 10));
    let mut stalled = Native {
        restored_at: 100,
        stalled_clock: true,
        ..target()
    };
    let report = execute(&mut stalled);
    assert_eq!(report.failure.unwrap().code, "restore_clock_stalled");
    assert!(report.restore_requested && !report.restored && !report.activation_attempted);
    assert_eq!(stalled.waits, [10]);
}

#[test]
fn refused_foreground_keeps_confirmed_restoration_and_the_activation_attempt() {
    let mut native = Native {
        foreground: false,
        ..target()
    };
    let report = execute(&mut native);
    assert_eq!(report.failure.unwrap().code, "activation_refused");
    assert!(report.restore_requested && report.restored && report.activation_attempted);
    assert!(!report.activation_succeeded);
    assert_eq!(
        native
            .journal
            .iter()
            .filter(|event| **event == "activate")
            .count(),
        1
    );
}
