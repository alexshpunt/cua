use super::*;
use serde_json::{json, Value};

fn request() -> Value {
    json!({"pid":42,"window_id":7,"from_x":5,"from_y":5,
        "via":[{"x":10,"y":20}],"to_x":20,"to_y":5,"delivery_mode":"foreground"})
}

#[test]
fn screenshot_coordinates_are_scaled_once_for_all_path_points() {
    let parsed = Request::parse(&request()).unwrap();
    assert_eq!((parsed.pid, parsed.window_id), (42, 7));
    let path = parsed.into_path(2.0).unwrap();
    assert_eq!(
        path.points,
        vec![
            Point { x: 10, y: 10 },
            Point { x: 20, y: 40 },
            Point { x: 40, y: 10 }
        ]
    );
    assert_eq!(path.duration_ms, 500);
}

#[test]
fn malformed_or_conflicting_requests_refuse_before_native_input() {
    let cases = [
        ("pid", json!(0), "invalid_path_target"),
        ("pid", json!(4294967296_u64), "invalid_path_target"),
        ("window_id", json!(0), "invalid_path_target"),
        ("scope", json!("desktop"), "invalid_path_scope"),
        ("steps", json!(20), "conflicting_path_fields"),
        ("modifier", json!("shift"), "conflicting_path_fields"),
        ("from_zoom", json!(true), "conflicting_path_fields"),
        ("button", json!("right"), "invalid_path_button"),
        ("from_x", json!(-1), "invalid_path_point"),
        ("from_y", Value::Null, "invalid_path_point"),
        ("via", json!([]), "invalid_path_length"),
        (
            "via",
            json!([{"x":1,"y":2,"extra":3}]),
            "invalid_path_point",
        ),
        ("via", json!([{"x":1}]), "invalid_path_point"),
        (
            "via",
            json!(vec![json!({"x":1,"y":1}); 255]),
            "invalid_path_length",
        ),
        ("duration_ms", json!(10001), "invalid_path_duration"),
        ("duration_ms", json!(-1), "invalid_path_duration"),
    ];
    for (key, value, code) in cases {
        let mut args = request();
        args[key] = value;
        assert_eq!(Request::parse(&args).err(), Some(code), "{key}");
    }
    for key in ["pid", "window_id"] {
        let mut args = request();
        args.as_object_mut().unwrap().remove(key);
        assert_eq!(Request::parse(&args).err(), Some("invalid_path_target"));
    }
}

#[test]
fn invalid_scaling_cannot_saturate_into_a_different_native_point() {
    for ratio in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        assert_eq!(
            Request::parse(&request())
                .unwrap()
                .into_path(ratio)
                .err()
                .map(|s| s),
            Some("invalid_path_scale")
        );
    }
    let mut args = request();
    args["to_x"] = json!(1e100);
    assert_eq!(
        Request::parse(&args)
            .unwrap()
            .into_path(2.0)
            .err()
            .map(|s| s),
        Some("invalid_path_point")
    );
}

#[test]
fn public_receipt_keeps_partial_prefix_without_claiming_release() {
    let result = Report {
        completed_points: 2,
        pressed: true,
        released: false,
        error: Some("release_failed".into()),
    }
    .into_tool_result(3, 500, 510);
    let public = result.action_record.unwrap().public_result().unwrap();
    let value = serde_json::to_value(public).unwrap();
    assert_eq!(value["effect"], "partial");
    assert_eq!(value["delivery"]["delivered_count"], 2);
    assert_eq!(value["delivery"]["mode"], "foreground");
    assert!(
        matches!(&result.content[0], cua_driver_core::protocol::Content::Text{text,..} if text.contains("released=false") && text.contains("2/3"))
    );
}
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Move(Point),
    Down,
    Up,
    Wait(u64),
}

#[derive(Default)]
struct Witness {
    events: Vec<Event>,
    fail_move: Option<usize>,
    moves: usize,
    fail_wait: bool,
    refuse: bool,
    release_failures: usize,
    panic_move: bool,
}

impl Backend for Witness {
    fn check_target(&mut self) -> Result<(), String> {
        if self.refuse {
            Err("target_lost".into())
        } else {
            Ok(())
        }
    }
    fn move_to(&mut self, point: Point) -> Result<(), String> {
        self.moves += 1;
        if self.panic_move && self.moves == 2 {
            panic!("native worker panic");
        }
        if self.fail_move == Some(self.moves) {
            return Err("input_failed".into());
        }
        self.events.push(Event::Move(point));
        Ok(())
    }
    fn press(&mut self) -> Result<(), String> {
        self.events.push(Event::Down);
        Ok(())
    }
    fn release(&mut self) -> Result<(), String> {
        self.events.push(Event::Up);
        if self.release_failures > 0 {
            self.release_failures -= 1;
            Err("release_failed".into())
        } else {
            Ok(())
        }
    }
    fn wait_until(&mut self, time: u64) -> Result<(), String> {
        if self.fail_wait {
            return Err("cancelled".into());
        }
        self.events.push(Event::Wait(time));
        Ok(())
    }
}

fn path() -> Path {
    Path::new(
        vec![
            Point { x: 10, y: 10 },
            Point { x: 20, y: 40 },
            Point { x: 40, y: 10 },
        ],
        60,
    )
    .unwrap()
}

#[test]
fn a_curve_keeps_one_press_across_every_intermediate_point() {
    let path = path();
    let mut witness = Witness::default();
    let result = execute(&path, &mut witness);
    assert_eq!(
        witness.events,
        vec![
            Event::Move(path.points[0]),
            Event::Down,
            Event::Wait(30),
            Event::Move(path.points[1]),
            Event::Wait(60),
            Event::Move(path.points[2]),
            Event::Up
        ]
    );
    assert_eq!(result.completed_points, 3);
    assert!(result.pressed && result.released);
    assert!(result.error.is_none());
}

#[test]
fn an_invalid_whole_path_is_rejected_before_a_backend_exists() {
    for points in [
        vec![],
        vec![Point { x: 0, y: 0 }],
        vec![Point { x: 0, y: 0 }; 257],
        vec![Point { x: 0, y: 0 }, Point { x: -1, y: 0 }],
    ] {
        assert!(Path::new(points, 500).is_err());
    }
    assert_eq!(
        Path::new(vec![Point { x: 0, y: 0 }; 2], 10001).err(),
        Some("invalid_path_duration")
    );
}

#[test]
fn target_refusal_has_no_pointer_input() {
    let mut witness = Witness {
        refuse: true,
        ..Default::default()
    };
    let result = execute(&path(), &mut witness);
    assert!(witness.events.is_empty());
    assert_eq!(result.error.as_deref(), Some("target_lost"));
    assert!(!result.pressed);
}

#[test]
fn a_failed_move_releases_and_reports_only_the_completed_prefix() {
    let mut witness = Witness {
        fail_move: Some(3),
        ..Default::default()
    };
    let result = execute(&path(), &mut witness);
    assert_eq!(result.completed_points, 2);
    assert_eq!(result.error.as_deref(), Some("input_failed"));
    assert!(result.released);
    assert_eq!(
        witness.events.iter().filter(|e| **e == Event::Down).count(),
        1
    );
    assert_eq!(
        witness.events.iter().filter(|e| **e == Event::Up).count(),
        1
    );
    assert_eq!(witness.events.last(), Some(&Event::Up));
}

#[test]
fn cancellation_during_a_wait_releases_without_later_movement() {
    let mut witness = Witness {
        fail_wait: true,
        ..Default::default()
    };
    let result = execute(&path(), &mut witness);
    assert_eq!(result.error.as_deref(), Some("cancelled"));
    assert_eq!(result.completed_points, 1);
    assert!(result.released);
    assert_eq!(
        witness.events,
        vec![Event::Move(path().points[0]), Event::Down, Event::Up]
    );
}

#[test]
fn a_release_failure_gets_cleanup_not_a_replayed_path() {
    let mut witness = Witness {
        release_failures: 1,
        ..Default::default()
    };
    let result = execute(&path(), &mut witness);
    assert_eq!(result.completed_points, 3);
    assert_eq!(result.error.as_deref(), Some("release_failed"));
    assert!(
        !result.released,
        "failed release receipt must not claim confirmation from best-effort Drop"
    );
    assert_eq!(
        witness.events.iter().filter(|e| **e == Event::Down).count(),
        1
    );
    assert_eq!(
        witness.events.iter().filter(|e| **e == Event::Up).count(),
        2
    );
}

#[test]
fn unwinding_the_native_worker_still_releases_its_press() {
    let mut witness = Witness {
        panic_move: true,
        ..Default::default()
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(&path(), &mut witness)
    }));
    assert!(result.is_err());
    assert_eq!(witness.events.last(), Some(&Event::Up));
}
