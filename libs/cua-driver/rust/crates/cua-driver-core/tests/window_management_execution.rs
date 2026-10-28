use cua_driver_contract::*;
use cua_driver_core::window_management::{self, Backend, Failure};
use std::collections::VecDeque;

fn sample() -> WindowControlSample {
    WindowControlSample {
        bounds: DesktopWindowBounds {
            x: -20,
            y: 30,
            width: 640,
            height: 480,
        },
        bounds_source: WindowBoundsSource::DwmVisibleFrame,
        coordinate_space: WindowManagementSpace::WindowsDesktopPixels,
        minimized: false,
        maximized: false,
        visible: true,
        foreground_at_target: false,
        keyboard_focus_at_target: None,
        foreground_window: 99,
        virtual_desktop: VirtualDesktopMembership::from_queries(
            Ok("12345678-1234-1234-1234-123456789abc".into()),
            Ok(true),
            1,
        ),
    }
}
fn input(operation: WindowManagementOperation) -> WindowManagementInput {
    WindowManagementInput {
        pid: 42,
        window_id: 73,
        state_id: "state".into(),
        operation,
        x: if operation == WindowManagementOperation::Move {
            Some(200)
        } else {
            None
        },
        y: if operation == WindowManagementOperation::Move {
            Some(300)
        } else {
            None
        },
        width: if operation == WindowManagementOperation::Resize {
            Some(800)
        } else {
            None
        },
        height: if operation == WindowManagementOperation::Resize {
            Some(600)
        } else {
            None
        },
        unit: if matches!(
            operation,
            WindowManagementOperation::Move | WindowManagementOperation::Resize
        ) {
            Some(WindowManagementSpace::WindowsDesktopPixels)
        } else {
            None
        },
        session: None,
    }
}
fn failure(code: &'static str) -> Failure {
    Failure {
        code,
        detail: code.into(),
    }
}
struct Fake {
    current: WindowControlSample,
    samples: VecDeque<Result<WindowControlSample, Failure>>,
    calls: Vec<&'static str>,
    requests: Vec<(WindowManagementOperation, Option<DesktopWindowBounds>)>,
    clock: u64,
    stalled: bool,
    checks: usize,
    fail_check: Option<usize>,
    request_error: Option<Failure>,
    wait_error: Option<Failure>,
    resize_tolerance_px: u32,
}
impl Fake {
    fn new() -> Self {
        Self {
            current: sample(),
            samples: VecDeque::new(),
            calls: vec![],
            requests: vec![],
            clock: 0,
            stalled: false,
            checks: 0,
            fail_check: None,
            request_error: None,
            wait_error: None,
            resize_tolerance_px: 0,
        }
    }
}
impl Backend for Fake {
    fn resize_tolerance_px(&mut self) -> u32 {
        self.resize_tolerance_px
    }
    fn now_ms(&self) -> u64 {
        self.clock
    }
    fn check_target(&mut self) -> Result<(), Failure> {
        self.calls.push("check");
        self.checks += 1;
        if self.fail_check == Some(self.checks) {
            Err(failure("cancelled"))
        } else {
            Ok(())
        }
    }
    fn sample(&mut self) -> Result<WindowControlSample, Failure> {
        self.calls.push("sample");
        self.samples
            .pop_front()
            .unwrap_or_else(|| Ok(self.current.clone()))
    }
    fn request(
        &mut self,
        operation: WindowManagementOperation,
        bounds: Option<DesktopWindowBounds>,
    ) -> Result<(), Failure> {
        self.calls.push("request");
        self.requests.push((operation, bounds));
        if let Some(error) = self.request_error.take() {
            return Err(error);
        }
        match operation {
            WindowManagementOperation::Move | WindowManagementOperation::Resize => {
                self.current.bounds = bounds.unwrap()
            }
            WindowManagementOperation::Minimize => self.current.minimized = true,
            WindowManagementOperation::Maximize => self.current.maximized = true,
            WindowManagementOperation::Focus => {
                self.current.foreground_at_target = true;
                self.current.foreground_window = 73
            }
        }
        Ok(())
    }
    fn wait_ms(&mut self, ms: u64) -> Result<(), Failure> {
        self.calls.push("wait");
        assert!(ms <= 10);
        if let Some(error) = self.wait_error.take() {
            return Err(error);
        }
        if !self.stalled {
            self.clock += ms
        }
        Ok(())
    }
}

#[test]
fn invalid_inputs_never_touch_backend_and_frames_preserve_unrequested_components() {
    let mut backend = Fake::new();
    let mut invalid = input(WindowManagementOperation::Move);
    invalid.width = Some(1);
    assert_eq!(
        window_management::execute(&invalid, &mut backend)
            .failure
            .unwrap()
            .code,
        "invalid_arguments"
    );
    assert!(backend.calls.is_empty());
    for operation in [
        WindowManagementOperation::Move,
        WindowManagementOperation::Resize,
        WindowManagementOperation::Minimize,
        WindowManagementOperation::Maximize,
        WindowManagementOperation::Focus,
    ] {
        let mut backend = Fake::new();
        let before = backend.current.clone();
        let report = window_management::execute(&input(operation), &mut backend);
        assert_eq!(report.failure, None);
        assert!(
            report.mutation_attempted && report.mutation_accepted && report.postcondition_confirmed
        );
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(report.before, Some(before.clone()));
        let after = report.observed.unwrap();
        if operation == WindowManagementOperation::Move {
            assert_eq!(
                after.bounds,
                DesktopWindowBounds {
                    x: 200,
                    y: 300,
                    ..before.bounds
                }
            );
        }
        if operation == WindowManagementOperation::Resize {
            assert_eq!(
                after.bounds,
                DesktopWindowBounds {
                    width: 800,
                    height: 600,
                    ..before.bounds
                }
            );
        }
        if operation != WindowManagementOperation::Focus {
            assert_eq!(after.foreground_at_target, before.foreground_at_target);
            assert_eq!(
                after.keyboard_focus_at_target,
                before.keyboard_focus_at_target
            );
        }
    }
}

#[test]
fn forbidden_states_and_unknown_or_other_desktops_refuse_before_mutation() {
    for (operation, min, max, code) in [
        (
            WindowManagementOperation::Move,
            true,
            false,
            "window_not_normal",
        ),
        (
            WindowManagementOperation::Resize,
            false,
            true,
            "window_not_normal",
        ),
        (
            WindowManagementOperation::Maximize,
            true,
            false,
            "window_minimized",
        ),
        (
            WindowManagementOperation::Focus,
            true,
            false,
            "window_minimized",
        ),
        (
            WindowManagementOperation::Minimize,
            true,
            false,
            "window_already_minimized",
        ),
        (
            WindowManagementOperation::Maximize,
            false,
            true,
            "window_already_maximized",
        ),
    ] {
        let mut backend = Fake::new();
        backend.current.minimized = min;
        backend.current.maximized = max;
        let report = window_management::execute(&input(operation), &mut backend);
        assert_eq!(report.failure.unwrap().code, code);
        assert!(!report.mutation_attempted);
        assert!(backend.requests.is_empty());
    }
    for operation in [
        WindowManagementOperation::Maximize,
        WindowManagementOperation::Focus,
    ] {
        for known in [Some(false), None] {
            let mut backend = Fake::new();
            backend.current.virtual_desktop = VirtualDesktopMembership::from_queries(
                Ok("12345678-1234-1234-1234-123456789abc".into()),
                known.ok_or(DesktopQueryError {
                    code: DesktopQueryErrorCode::ManagerUnavailable,
                    hresult: None,
                }),
                1,
            );
            let report = window_management::execute(&input(operation), &mut backend);
            assert_eq!(
                report.failure.unwrap().code,
                if known.is_some() {
                    "window_not_on_current_desktop"
                } else {
                    "window_desktop_unknown"
                }
            );
            assert!(backend.requests.is_empty());
        }
    }
}

#[test]
fn uncertain_request_and_queries_stop_without_replay_and_keep_prefixes() {
    let mut backend = Fake::new();
    backend.samples.push_back(Err(failure("query_failed")));
    let report = window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
    assert_eq!(report.failure.unwrap().code, "query_failed");
    assert!(!report.mutation_attempted);
    let mut backend = Fake::new();
    backend.request_error = Some(failure("unknown"));
    let report = window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
    assert_eq!(report.failure.unwrap().code, "unknown");
    assert!(report.mutation_attempted);
    assert!(!report.mutation_accepted);
    assert_eq!(backend.requests.len(), 1);
    assert_eq!(backend.calls.last(), Some(&"request"));
    let mut backend = Fake::new();
    backend
        .samples
        .extend([Ok(sample()), Err(failure("query_failed"))]);
    let report = window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
    assert_eq!(report.failure.unwrap().code, "query_failed");
    assert!(report.mutation_accepted);
    assert_eq!(backend.requests.len(), 1);
}

#[test]
fn cancellation_at_every_check_preserves_completed_prefix_and_never_replays() {
    let mut baseline = Fake::new();
    let result = window_management::execute(&input(WindowManagementOperation::Move), &mut baseline);
    assert!(result.postcondition_confirmed);
    for check in 1..=baseline.checks {
        let mut backend = Fake::new();
        backend.fail_check = Some(check);
        let report =
            window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
        assert_eq!(report.failure.unwrap().code, "cancelled");
        assert!(backend.requests.len() <= 1);
        assert_eq!(report.mutation_attempted, !backend.requests.is_empty());
        if check == baseline.checks {
            assert!(report.mutation_accepted && report.postcondition_confirmed);
            assert!(report.observed.is_some());
        }
    }
}

#[test]
fn constrained_or_changed_source_geometry_times_out_with_last_observation() {
    for source_flip in [false, true] {
        let mut backend = Fake::new();
        let mut observed = sample();
        if source_flip {
            observed.bounds = DesktopWindowBounds {
                x: 200,
                y: 300,
                ..observed.bounds
            };
            observed.bounds_source = WindowBoundsSource::WindowRect;
        }
        backend.samples.push_back(Ok(sample()));
        for _ in 0..110 {
            backend.samples.push_back(Ok(observed.clone()));
        }
        let report =
            window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
        assert_eq!(report.failure.unwrap().code, "window_management_timeout");
        assert!(report.mutation_accepted);
        assert!(!report.postcondition_confirmed);
        assert_eq!(report.observed, Some(observed));
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.clock, 1000);
    }
}

#[test]
fn stalled_clock_and_cancelled_wait_are_bounded_and_keep_last_observation() {
    for stalled in [true, false] {
        let mut backend = Fake::new();
        backend.samples.extend([Ok(sample()), Ok(sample())]);
        backend.stalled = stalled;
        if !stalled {
            backend.wait_error = Some(failure("cancelled"));
        }
        let report =
            window_management::execute(&input(WindowManagementOperation::Move), &mut backend);
        assert_eq!(
            report.failure.unwrap().code,
            if stalled {
                "window_management_clock_stalled"
            } else {
                "cancelled"
            }
        );
        assert!(report.mutation_accepted);
        assert_eq!(report.observed, Some(sample()));
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(
            backend.calls.iter().filter(|&&call| call == "wait").count(),
            1
        );
    }
}

/// LPT-940: a DPI-unaware window on a scaled display lands on whole logical pixels, so Windows can
/// settle a pixel away from the requested size. The backend names how far; position stays exact.
fn resize_with_observed(
    tolerance: u32,
    operation: WindowManagementOperation,
    observed: DesktopWindowBounds,
) -> (window_management::Report, Fake) {
    let mut backend = Fake::new();
    backend.resize_tolerance_px = tolerance;
    let mut settled = sample();
    settled.bounds = observed;
    backend.samples.push_back(Ok(sample()));
    for _ in 0..110 {
        backend.samples.push_back(Ok(settled.clone()));
    }
    let report = window_management::execute(&input(operation), &mut backend);
    (report, backend)
}
fn requested_size(width: u32, height: u32) -> DesktopWindowBounds {
    DesktopWindowBounds {
        width,
        height,
        ..sample().bounds
    }
}

#[test]
fn a_resize_within_the_allowed_size_difference_is_confirmed_and_shows_both_bounds() {
    for (width, height) in [(801, 600), (799, 600), (800, 601), (801, 599)] {
        let (report, backend) = resize_with_observed(
            1,
            WindowManagementOperation::Resize,
            requested_size(width, height),
        );
        assert!(report.failure.is_none(), "{width}x{height}");
        assert!(report.postcondition_confirmed);
        assert_eq!(report.requested_bounds, Some(requested_size(800, 600)));
        assert_eq!(
            report.observed.unwrap().bounds,
            requested_size(width, height)
        );
        assert_eq!(backend.requests.len(), 1);
    }
}

#[test]
fn a_resize_outside_the_allowed_difference_still_times_out_after_the_same_wait() {
    for (tolerance, width, height) in [(1, 802, 600), (1, 800, 598), (0, 801, 600)] {
        let (report, backend) = resize_with_observed(
            tolerance,
            WindowManagementOperation::Resize,
            requested_size(width, height),
        );
        assert_eq!(report.failure.unwrap().code, "window_management_timeout");
        assert!(report.mutation_accepted);
        assert!(!report.postcondition_confirmed);
        assert_eq!(
            report.observed.unwrap().bounds,
            requested_size(width, height)
        );
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.clock, 1000);
    }
}

#[test]
fn the_allowed_size_difference_never_loosens_position_or_a_move() {
    let shifted = DesktopWindowBounds {
        x: sample().bounds.x + 1,
        ..requested_size(800, 600)
    };
    let (report, _) = resize_with_observed(1, WindowManagementOperation::Resize, shifted);
    assert_eq!(report.failure.unwrap().code, "window_management_timeout");
    let nearly = DesktopWindowBounds {
        x: 201,
        y: 300,
        ..sample().bounds
    };
    let (report, _) = resize_with_observed(1, WindowManagementOperation::Move, nearly);
    assert_eq!(report.failure.unwrap().code, "window_management_timeout");
}

#[test]
fn the_size_difference_follows_how_far_windows_scales_the_window_for_its_display() {
    use window_management::virtualized_resize_tolerance_px as tolerance;
    // ZinPC: a 96 DPI (100%) window on a 150% display reached 593 for 592 and 599 for 598.
    assert_eq!(tolerance(100, 150), 1);
    // A window that draws at the display's own scale, or at a larger one, takes exact sizes.
    assert_eq!(tolerance(150, 150), 0);
    assert_eq!(tolerance(100, 100), 0);
    assert_eq!(tolerance(200, 100), 0);
    // The nearest reachable size is at most 1 px away up to a 300% display, 2 px from 350% to 500%.
    assert_eq!(
        [125, 200, 300].map(|monitor| tolerance(100, monitor)),
        [1, 1, 1]
    );
    assert_eq!(
        [350, 400, 500].map(|monitor| tolerance(100, monitor)),
        [2, 2, 2]
    );
    // An unknown scale explains nothing.
    assert_eq!(tolerance(0, 150), 0);
    assert_eq!(tolerance(100, 0), 0);
}
