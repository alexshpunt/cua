// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Native pointer posture and receiver-owned state across legacy restoration seams.

use super::*;
use windows::Win32::UI::WindowsAndMessaging::{GetPhysicalCursorPos, SetPhysicalCursorPos};

pub(super) struct PhysicalCoordinates(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

impl PhysicalCoordinates {
    pub(super) fn enter() -> Self {
        use windows::Win32::UI::HiDpi::{
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        assert!(
            !previous.0.is_null(),
            "physical-pixel observer context is required"
        );
        Self(previous)
    }
}

impl Drop for PhysicalCoordinates {
    fn drop(&mut self) {
        unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.0) };
    }
}
struct PointerPair {
    driver: McpDriver,
    _temporary: tempfile::TempDir,
    target_path: PathBuf,
    human_path: PathBuf,
    target_pid: u32,
    target_wid: u64,
    human_wid: u64,
    target_snapshot: ToolResponse,
    local_canvas: (f64, f64, f64, f64),
    screen_canvas: (f64, f64, f64, f64),
}

impl PointerPair {
    fn launch(label: &str, busy_ms: u32, trigger: &str) -> Self {
        Self::launch_with_preparation(label, busy_ms, trigger, false)
    }

    fn launch_with_preparation(label: &str, busy_ms: u32, trigger: &str, fresh_key: bool) -> Self {
        let temporary = tempfile::tempdir().expect("pointer state directory");
        let mut driver = McpDriver::spawn_named_with_env(label, &[("CUA_FOCUS_RETURN", "1")])
            .expect("required Windows driver did not start");
        let state_root = driver
            .recording_dir()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| temporary.path().to_path_buf());
        let target_path = state_root.join("pointer-target-state.json");
        let human_path = state_root.join("pointer-human-state.json");
        let (target_pid, target_wid) =
            launch_pointer_receiver(&mut driver, &target_path, "target", busy_ms, false, trigger);
        let (human_pid, human_wid) =
            launch_pointer_receiver(&mut driver, &human_path, "human", 0, false, "held_move");
        assert_ne!(target_pid, human_pid);
        let human = snapshot(&mut driver, human_pid, human_wid);
        let human_canvas = screen_canvas(&human);
        let human_point = fraction_point(human_canvas, 0.25);
        if fresh_key {
            prepare_human_with_observed_input(human_wid, human_point, &human_path);
            driver.start_behavior_recording();
        } else {
            focus_harness(&mut driver, human_pid, human_wid);
        }
        let target_bounds = window_bounds(&mut driver, target_pid, target_wid);
        // Publish the target's capture last so the coordinate tools use current authority.
        let target_snapshot = snapshot(&mut driver, target_pid, target_wid);
        assert!(!target_snapshot.is_error(), "{}", target_snapshot.text());
        let local_canvas = pixel_frame(&target_snapshot, "pointer-canvas", target_bounds);
        let screen_canvas = screen_canvas(&target_snapshot);
        if !fresh_key {
            unsafe { SetPhysicalCursorPos(human_point.0, human_point.1) }
                .expect("place the fixture cursor inside the human canvas");
        }
        assert_eq!(foreground(), human_wid);
        Self {
            driver,
            _temporary: temporary,
            target_path,
            human_path,
            target_pid,
            target_wid,
            human_wid,
            target_snapshot,
            local_canvas,
            screen_canvas,
        }
    }

    fn local_point(&self, fraction: f64) -> (f64, f64) {
        let (x, y, width, height) = self.local_canvas;
        (x + width * fraction, y + height / 2.0)
    }

    fn assert_posture(&self, fraction: f64) {
        assert_eq!(foreground(), self.target_wid, "target lost foreground");
        let expected = fraction_point(self.screen_canvas, fraction);
        let mut actual = POINT::default();
        unsafe { GetPhysicalCursorPos(&mut actual) }.expect("read physical cursor position");
        assert!(
            (actual.x - expected.0).abs() <= 2 && (actual.y - expected.1).abs() <= 2,
            "cursor left injected destination: actual=({}, {}), expected={expected:?}",
            actual.x,
            actual.y
        );
    }

    fn click(&mut self, fraction: f64) -> ToolResponse {
        let (x, y) = self.local_point(fraction);
        self.driver.call(
            "click",
            serde_json::json!({
                "pid": self.target_pid, "window_id": self.target_wid,
                "x": x, "y": y, "delivery_mode": "foreground"
            }),
        )
    }

    fn assert_human_received_no_pointer_input(&self, not_before_ms: u64) {
        let human = wait_pointer_receiver(&self.human_path, not_before_ms, |_| true);
        assert_eq!(human["down"], 0, "DOWN leaked: {human}");
        assert_eq!(human["up"], 0, "UP leaked: {human}");
        assert_eq!(human["wheel"], 0, "wheel leaked: {human}");
        assert_eq!(human["dropped_events"], 0);
    }
}

/// Establish the fresh-key control without an unobservable driver unlock key.
/// The parent inserts fixture setup input and waits for its receiver-owned UP.
fn prepare_human_with_observed_input(hwnd: u64, point: (i32, i32), state: &std::path::Path) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, WindowFromPoint, GA_ROOT, HWND_NOTOPMOST, SWP_SHOWWINDOW,
    };
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        for band in [HWND_TOPMOST, HWND_NOTOPMOST] {
            SetWindowPos(
                hwnd,
                Some(band),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
            .expect("raise the owned fixture without an unlock key");
        }
        SetPhysicalCursorPos(point.0, point.1).expect("place the owned setup pointer");
        let hit = WindowFromPoint(POINT {
            x: point.0,
            y: point.1,
        });
        assert_eq!(
            GetAncestor(hit, GA_ROOT),
            hwnd,
            "owned setup point is covered"
        );
    }
    let input = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inserted = unsafe {
        SendInput(
            &[input(MOUSEEVENTF_LEFTDOWN), input(MOUSEEVENTF_LEFTUP)],
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if inserted == 1 {
        // Only our DOWN was inserted; balance it without replaying the press.
        let _ = unsafe {
            SendInput(
                &[input(MOUSEEVENTF_LEFTUP)],
                std::mem::size_of::<INPUT>() as i32,
            )
        };
    }
    assert_eq!(inserted, 2, "owned setup click must be fully inserted");
    let observed = wait_pointer_receiver(state, 0, |state| state["down"] == 1 && state["up"] == 1);
    assert_eq!(observed["dropped_events"], 0);
    assert_eq!(foreground(), hwnd.0 as usize as u64);
}

fn foreground() -> u64 {
    unsafe { GetForegroundWindow() }.0 as usize as u64
}

fn screen_canvas(snapshot: &ToolResponse) -> (f64, f64, f64, f64) {
    let index = ax::element_index_by_id(snapshot.tree_text(), "pointer-canvas")
        .expect("pointer canvas in UIA snapshot");
    let frame = &snapshot.structured()["elements"]
        .as_array()
        .expect("structured UIA elements")
        .iter()
        .find(|element| element["element_index"].as_u64() == Some(index))
        .expect("pointer canvas geometry")["frame"];
    (
        frame["x"].as_f64().expect("canvas X"),
        frame["y"].as_f64().expect("canvas Y"),
        frame["w"].as_f64().expect("canvas width"),
        frame["h"].as_f64().expect("canvas height"),
    )
}

fn fraction_point(canvas: (f64, f64, f64, f64), fraction: f64) -> (i32, i32) {
    let (x, y, width, height) = canvas;
    (
        (x + width * fraction).round() as i32,
        (y + height / 2.0).round() as i32,
    )
}

fn assert_unobservable(response: &ToolResponse) {
    assert!(!response.is_error(), "{}", response.text());
    assert_eq!(response.action_effect(), Some("unverifiable"));
}

fn run_pointer_owner(
    action: &str,
    busy_ms: u32,
    trigger: &str,
    test: impl FnOnce(&mut PointerPair),
) {
    let mut case =
        native_foreground_case("wpf", action, Targeting::Px, DriverRoute::WindowsSendInput);
    case.oracles.extend([
        OracleKind::Focus,
        OracleKind::Cursor,
        OracleKind::NoLeakedInput,
    ]);
    let cell_id = case.cell_id.clone();
    execute_case(case, |evidence| {
        let _coordinates = PhysicalCoordinates::enter();
        let mut pair = PointerPair::launch(&cell_id, busy_ms, trigger);
        *evidence = recording_evidence(pair.driver.recording_dir());
        test(&mut pair);
        drop(pair);
        Observation::delivered(
            vec![
                OracleKind::FixtureState,
                OracleKind::Focus,
                OracleKind::Cursor,
                OracleKind::NoLeakedInput,
            ],
            Evidence::default(),
        )
    });
}

#[test]
#[ignore]
fn harness_wpf_supported_key_read_returns_the_human_window() {
    let mut case = native_foreground_case(
        "wpf",
        "supported_key_read_return",
        Targeting::NotApplicable,
        DriverRoute::WindowsSendInput,
    );
    case.oracles
        .extend([OracleKind::Focus, OracleKind::NoLeakedInput]);
    let cell_id = case.cell_id.clone();
    execute_case(case, |evidence| {
        let _coordinates = PhysicalCoordinates::enter();
        let mut pair = PointerPair::launch_with_preparation(&cell_id, 0, "held_move", true);
        *evidence = recording_evidence(pair.driver.recording_dir());
        let response = pair.driver.call(
            "press_key",
            serde_json::json!({
                "pid": pair.target_pid, "window_id": pair.target_wid,
                "key": "f1", "delivery_mode": "foreground"
            }),
        );
        assert!(!response.is_error(), "{}", response.text());
        assert_eq!(foreground(), pair.human_wid);
        let finished = pointer_oracle_time_ms();
        let target = wait_pointer_receiver(&pair.target_path, finished, |state| {
            state["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["message"] == 0x0101 && event["virtual_key"] == 0x70)
        });
        let human = wait_pointer_receiver(&pair.human_path, finished, |_| true);
        for message in [0x0100, 0x0101] {
            let count = |state: &serde_json::Value| {
                state["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event["message"] == message && event["virtual_key"] == 0x70)
                    .count()
            };
            assert_eq!(count(&target), 1, "target key journal: {target}");
            assert_eq!(count(&human), 0, "key leaked to the human: {human}");
        }
        assert_eq!(target["down"], 0);
        assert_eq!(target["up"], 0);
        assert_eq!(human["down"], 1, "only the externally observed setup click");
        assert_eq!(human["up"], 1);
        drop(pair);
        Observation::delivered(
            vec![
                OracleKind::FixtureState,
                OracleKind::Focus,
                OracleKind::NoLeakedInput,
            ],
            Evidence::default(),
        )
    });
}
#[test]
#[ignore]
fn harness_wpf_legacy_click_keeps_unread_pointer_at_target() {
    run_pointer_owner("legacy_click_unread_posture", 1200, "down", |pair| {
        let response = pair.click(0.75);
        assert!(!response.is_error(), "{}", response.text());
        let finished_ms = pointer_oracle_time_ms();
        pair.assert_posture(0.75);
        let target = wait_pointer_receiver(&pair.target_path, finished_ms, |state| {
            state["busy_finished"] == true && state["up"] == 1
        });
        assert_eq!(target["busy_trigger"], "down");
        assert_eq!(target["down"], 1);
        assert_eq!(target["wheel"], 0);
        assert_eq!(target["dropped_events"], 0);
        let events = target["events"].as_array().expect("target event journal");
        let busy_end = events
            .iter()
            .position(|event| event["kind"] == "busy_end")
            .expect("completed receiver work");
        let up = events
            .iter()
            .position(|event| event["kind"] == "up")
            .expect("target UP");
        assert!(busy_end < up, "UP must wait for receiver work: {target}");
        assert_eq!(events[up]["foreground_window"], pair.target_wid);
        pair.assert_human_received_no_pointer_input(finished_ms);
        pair.assert_posture(0.75);
        assert_unobservable(&response);
    });
}

#[test]
#[ignore]
fn harness_wpf_straight_drag_keeps_unread_release_at_target() {
    run_pointer_owner("straight_drag_unread_posture", 1200, "down", |pair| {
        let (from_x, from_y) = pair.local_point(0.25);
        let (to_x, to_y) = pair.local_point(0.75);
        // No `via`: this must exercise the legacy straight-drag actuator,
        // not the held-path state machine owned by the parent test.
        let response = pair.driver.call(
            "drag",
            serde_json::json!({
                "pid": pair.target_pid, "window_id": pair.target_wid,
                "from_x": from_x, "from_y": from_y,
                "to_x": to_x, "to_y": to_y,
                "duration_ms": 400, "steps": 10, "delivery_mode": "foreground"
            }),
        );
        let finished_ms = pointer_oracle_time_ms();
        assert!(!response.is_error(), "{}", response.text());
        pair.assert_posture(0.75);
        let target = wait_pointer_receiver(&pair.target_path, finished_ms, |state| {
            state["busy_finished"] == true && state["up"] == 1
        });
        assert_eq!(target["down"], 1);
        assert_eq!(target["wheel"], 0);
        assert_eq!(target["dropped_events"], 0);
        let up = target["events"]
            .as_array()
            .expect("target event journal")
            .iter()
            .find(|event| event["kind"] == "up")
            .expect("target UP");
        assert_eq!(up["foreground_window"], pair.target_wid);
        pair.assert_human_received_no_pointer_input(finished_ms);
        pair.assert_posture(0.75);
        assert_unobservable(&response);
    });
}

#[test]
#[ignore]
fn harness_wpf_legacy_wheel_keeps_pointer_over_busy_receiver() {
    run_pointer_owner("legacy_wheel_unread_posture", 1200, "wheel", |pair| {
        let initial = pair.click(0.25);
        assert!(!initial.is_error(), "{}", initial.text());
        let (x, y) = pair.local_point(0.75);
        let response = pair.driver.call(
            "scroll",
            serde_json::json!({
                "pid": pair.target_pid, "window_id": pair.target_wid,
                "x": x, "y": y, "direction": "down", "amount": 1,
                "delivery_mode": "foreground"
            }),
        );
        let finished_ms = pointer_oracle_time_ms();
        // The prior cursor was at 25%; a timed wheel restore would put it back there.
        pair.assert_posture(0.75);
        let target = wait_pointer_receiver(&pair.target_path, finished_ms, |state| {
            state["busy_finished"] == true && state["up"] == 1 && state["wheel"] == 1
        });
        assert_eq!(target["down"], 1);
        assert_eq!(target["dropped_events"], 0);
        let events = target["events"].as_array().expect("target event journal");
        let busy_end = events
            .iter()
            .position(|event| event["kind"] == "busy_end")
            .expect("completed receiver work");
        let wheel = events
            .iter()
            .position(|event| event["kind"] == "wheel")
            .expect("target wheel");
        assert!(
            wheel < busy_end,
            "receiver did not run its wheel workload: {target}"
        );
        assert_eq!(events[wheel]["wheel_delta"], -120);
        assert_eq!(events[wheel]["foreground_window"], pair.target_wid);
        pair.assert_human_received_no_pointer_input(finished_ms);
        pair.assert_posture(0.75);
        assert_unobservable(&response);
    });
}

#[test]
#[ignore]
fn harness_wpf_native_pointer_tools_keep_their_measured_posture() {
    for action in ["move_pointer", "scroll_at"] {
        run_pointer_owner(&format!("{action}_no_return"), 0, "held_move", |pair| {
            let (x, y) = pair.local_point(0.75);
            let capture_id = pair.target_snapshot.structured()["capture_id"]
                .as_str()
                .expect("current exact-window capture authority");
            let mut input = serde_json::json!({
                "pid": pair.target_pid, "window_id": pair.target_wid,
                "capture_id": capture_id, "x": x, "y": y,
                "delivery_mode": "foreground"
            });
            if action == "move_pointer" {
                input["dwell_ms"] = serde_json::json!(0);
            } else {
                input["delta_x"] = serde_json::json!(0);
                input["delta_y"] = serde_json::json!(1);
                input["unit"] = serde_json::json!("wheel_detents");
            }
            let response = pair.driver.call(action, input);
            assert!(!response.is_error(), "{}", response.text());
            let finished_ms = pointer_oracle_time_ms();
            pair.assert_posture(0.75);
            let target = wait_pointer_receiver(&pair.target_path, finished_ms, |state| {
                if action == "move_pointer" {
                    state["moves"].as_u64().is_some_and(|moves| moves > 0)
                } else {
                    state["wheel"] == 1
                }
            });
            assert_eq!(target["down"], 0);
            assert_eq!(target["up"], 0);
            assert_eq!(target["dropped_events"], 0);
            pair.assert_human_received_no_pointer_input(finished_ms);
            pair.assert_posture(0.75);
            assert_unobservable(&response);
        });
    }
}
