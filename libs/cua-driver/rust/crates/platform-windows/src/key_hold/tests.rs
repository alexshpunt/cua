use super::*;
use crate::drag_path::{Backend as PathBackend, Point};

#[derive(Default)]
struct Fake {
    log: Vec<String>,
    held: Vec<String>,
    fail_key: Option<String>,
    fail_wait: bool,
    fail_pointer: bool,
}
impl PathBackend for Fake {
    fn check_target(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn move_to(&mut self, p: Point) -> Result<(), String> {
        self.log.push(format!("move:{},{}", p.x, p.y));
        Ok(())
    }
    fn press(&mut self) -> Result<(), String> {
        self.log.push("mouse_down".into());
        Ok(())
    }
    fn release(&mut self) -> Result<(), String> {
        self.log.push("mouse_up".into());
        Ok(())
    }
    fn wait_until(&mut self, _: u64) -> Result<(), String> {
        if self.fail_pointer {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }
}
impl Backend for Fake {
    fn press_key(&mut self, key: &str) -> Result<(), String> {
        if self.fail_key.as_deref() == Some(key) {
            return Err("input_failed".into());
        }
        self.log.push(format!("down:{key}"));
        self.held.push(key.into());
        Ok(())
    }
    fn release_keys(&mut self) -> Result<(), String> {
        for key in self.held.drain(..).rev() {
            self.log.push(format!("up:{key}"));
        }
        Ok(())
    }
    fn wait_hold(&mut self, duration: u64) -> Result<(), String> {
        self.log.push(format!("hold:{duration}"));
        if self.fail_wait {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }
}
fn request() -> serde_json::Value {
    serde_json::json!({"pid":42,"window_id":100,"keys":["w","shift"],"duration_ms":500})
}
#[test]
fn modifiers_go_down_before_letters_and_all_keys_release_in_reverse() {
    let hold = Request::parse(&request()).unwrap().into_hold(1.0).unwrap();
    let mut native = Fake::default();
    let result = execute(&hold, &mut native);
    assert!(result.error.is_none());
    assert!(result.keys_released);
    assert_eq!(
        native.log,
        ["down:shift", "down:w", "hold:500", "up:w", "up:shift"]
    );
}
#[test]
fn cancellation_releases_injected_keys_without_replaying() {
    let hold = Request::parse(&request()).unwrap().into_hold(1.0).unwrap();
    let mut native = Fake {
        fail_wait: true,
        ..Default::default()
    };
    let result = execute(&hold, &mut native);
    assert_eq!(result.error.as_deref(), Some("cancelled"));
    assert!(result.keys_released);
    assert!(native.held.is_empty());
    assert_eq!(
        native.log.iter().filter(|s| s.starts_with("down:")).count(),
        2
    );
}
#[test]
fn a_failed_second_key_releases_only_the_successful_first_key() {
    let hold = Request::parse(&request()).unwrap().into_hold(1.0).unwrap();
    let mut native = Fake {
        fail_key: Some("w".into()),
        ..Default::default()
    };
    let result = execute(&hold, &mut native);
    assert!(result.error.is_some());
    assert_eq!(result.pressed_keys, ["shift"]);
    assert_eq!(native.log, ["down:shift", "up:shift"]);
}
#[test]
fn curved_pointer_work_is_inside_one_key_hold() {
    let mut raw = request();
    raw["actions"] = serde_json::json!([{"action":"drag","from_x":1,"from_y":2,"via":[{"x":4,"y":9}],"to_x":8,"to_y":3,"duration_ms":200}]);
    let hold = Request::parse(&raw).unwrap().into_hold(2.0).unwrap();
    let mut native = Fake::default();
    let result = execute(&hold, &mut native);
    assert_eq!(result.completed_actions, 1);
    assert!(result.keys_released);
    assert_eq!(
        native.log,
        [
            "down:shift",
            "down:w",
            "move:2,4",
            "mouse_down",
            "move:8,18",
            "move:16,6",
            "mouse_up",
            "hold:500",
            "up:w",
            "up:shift"
        ]
    );
}
#[test]
fn bad_key_or_overlong_pointer_work_refuses_the_whole_request() {
    for keys in [
        serde_json::json!([]),
        serde_json::json!(["w", "w"]),
        serde_json::json!(["hyper"]),
    ] {
        let mut raw = request();
        raw["keys"] = keys;
        assert!(Request::parse(&raw).is_err());
    }
    let mut raw = request();
    raw["actions"] = serde_json::json!([{"action":"click","x":1,"y":2},{"action":"drag","from_x":1,"from_y":2,"to_x":8,"to_y":3,"duration_ms":500}]);
    assert!(Request::parse(&raw).is_err());
    raw["duration_ms"] = serde_json::json!(10001);
    assert!(Request::parse(&raw).is_err());
}

#[test]
fn an_interrupted_pointer_prefix_releases_keys_and_skips_later_actions() {
    let mut raw = request();
    raw["actions"] = serde_json::json!([
        {"action":"drag","from_x":1,"from_y":2,"via":[{"x":4,"y":9}],"to_x":8,"to_y":3,"duration_ms":200},
        {"action":"click","x":2,"y":3}
    ]);
    let hold = Request::parse(&raw).unwrap().into_hold(1.0).unwrap();
    let mut native = Fake {
        fail_pointer: true,
        ..Default::default()
    };
    let result = execute(&hold, &mut native);
    assert_eq!(result.completed_actions, 0);
    assert_eq!(result.pointer_reports.len(), 1);
    assert_eq!(result.pointer_reports[0].completed_points, 1);
    assert!(result.pointer_reports[0].released);
    assert!(result.keys_released);
    assert!(native.held.is_empty());
    let wire = result.into_tool_result(&hold, 220);
    assert_ne!(wire.is_error, Some(true));
    assert_eq!(
        wire.action_record
            .as_ref()
            .unwrap()
            .public_result()
            .unwrap()
            .effect,
        cua_driver_contract::ActionEffect::Partial
    );
    assert!(
        matches!(&wire.content[0],cua_driver_core::protocol::Content::Text{text,..} if text.contains("pointer_prefix=[(1, true, true)]") && text.contains("keys_released=true"))
    );
}
