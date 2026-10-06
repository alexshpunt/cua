//! One bounded key hold with complete pointer gestures inside it.
use crate::drag_path::{self, Path, Point};
use serde_json::{json, Value};

pub(crate) struct Hold {
    pub keys: Vec<String>,
    pub duration_ms: u64,
    pub actions: Vec<Path>,
}

/// The native backend owns inserted keys and can release only its own input.
pub(crate) trait Backend: drag_path::Backend {
    fn press_key(&mut self, key: &str) -> Result<(), String>;
    fn release_keys(&mut self) -> Result<(), String>;
    fn wait_hold(&mut self, duration_ms: u64) -> Result<(), String>;
}

pub(crate) struct Report {
    pub pressed_keys: Vec<String>,
    pub keys_released: bool,
    pub held_ms: u64,
    pub completed_actions: usize,
    pub pointer_reports: Vec<drag_path::Report>,
    pub error: Option<String>,
}
impl Report {
    pub fn into_tool_result(
        self,
        hold: &Hold,
        elapsed_ms: u64,
    ) -> cua_driver_core::protocol::ToolResult {
        use cua_driver_core::action_record::{
            ActionEffect, ActionExecutionRecord, ActionTransport, ActualDelivery, RequestedDelivery,
        };
        use cua_driver_core::protocol::ToolResult;
        let summary = format!("Windows key hold: pressed={:?}, keys_released={}, pointer_actions={}/{}, duration_ms={}, held_ms={}, elapsed_ms={}, pointer_prefix={:?}. {}",
            self.pressed_keys, self.keys_released, self.completed_actions, hold.actions.len(), hold.duration_ms, self.held_ms, elapsed_ms,
            self.pointer_reports.iter().map(|r| (r.completed_points, r.pressed, r.released)).collect::<Vec<_>>(),
            self.error.as_deref().unwrap_or("Delivery is not proof of the app's result."));
        if self.pressed_keys.is_empty() && self.error.is_some() {
            return ToolResult::error(summary).with_structured(json!({"effect":"refused", "code":self.error.as_deref().unwrap().split(':').next().unwrap_or("input_failed")}));
        }
        let mut record = ActionExecutionRecord::new(
            if self.error.is_none() {
                ActionEffect::Unverifiable
            } else {
                ActionEffect::Partial
            },
            ActionTransport::WindowsSendInput,
            RequestedDelivery::Foreground,
        );
        record.actual_delivery = Some(ActualDelivery::Foreground);
        record.delivered_count = Some(self.pressed_keys.len() as u32);
        ToolResult::text(summary).with_action_record(record)
    }
}
struct HeldKeys<'a, B: Backend> {
    backend: &'a mut B,
    released: bool,
}
impl<B: Backend> Drop for HeldKeys<'_, B> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.backend.release_keys();
        }
    }
}

/// Never replay a partial key press or pointer prefix; cleanup runs on every exit.
pub(crate) fn execute(hold: &Hold, backend: &mut impl Backend) -> Report {
    let mut report = Report {
        pressed_keys: vec![],
        keys_released: false,
        held_ms: 0,
        completed_actions: 0,
        pointer_reports: vec![],
        error: None,
    };
    let mut guard = HeldKeys {
        backend,
        released: false,
    };
    for key in &hold.keys {
        if let Err(error) = guard
            .backend
            .check_target()
            .and_then(|()| guard.backend.press_key(key))
        {
            report.error = Some(error);
            break;
        }
        report.pressed_keys.push(key.clone());
    }
    if report.error.is_none() {
        for path in &hold.actions {
            let pointer = drag_path::execute(path, guard.backend);
            let error = pointer.error.clone();
            report.pointer_reports.push(pointer);
            if let Some(error) = error {
                report.error = Some(error);
                break;
            }
            report.completed_actions += 1;
        }
    }
    if report.error.is_none() {
        if let Err(error) = guard
            .backend
            .wait_hold(hold.duration_ms)
            .and_then(|()| guard.backend.check_target())
        {
            report.error = Some(error);
        }
    }
    match guard.backend.release_keys() {
        Ok(()) => {
            guard.released = true;
            report.keys_released = true;
        }
        Err(error) => {
            report.error = Some(match report.error.take() {
                Some(prior) => format!("{prior}; {error}"),
                None => error,
            })
        }
    }
    report
}

pub(crate) struct Request {
    pub pid: u32,
    pub window_id: u64,
    keys: Vec<String>,
    duration_ms: u64,
    actions: Vec<Value>,
}
fn modifier(key: &str) -> bool {
    matches!(key, "ctrl" | "shift" | "alt" | "win")
}
fn valid_key(key: &str) -> bool {
    modifier(key)
        || (key.len() == 1 && key.as_bytes()[0].is_ascii_lowercase())
        || (key.len() == 1 && key.as_bytes()[0].is_ascii_digit())
        || matches!(
            key,
            "return"
                | "tab"
                | "escape"
                | "space"
                | "backspace"
                | "delete"
                | "up"
                | "down"
                | "left"
                | "right"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
        )
        || (key.strip_prefix('f').is_some_and(|v| {
            v.parse::<u8>()
                .is_ok_and(|n| (1..=12).contains(&n) && n.to_string() == v)
        }))
}
fn coordinate(value: Option<&Value>) -> Result<f64, &'static str> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or("invalid_hold_point")
}
fn points(action: &Value) -> Result<(Vec<(f64, f64)>, u64), &'static str> {
    let object = action.as_object().ok_or("invalid_hold_action")?;
    let kind = action["action"].as_str().ok_or("invalid_hold_action")?;
    let allowed = if kind == "click" {
        vec!["action", "x", "y"]
    } else if kind == "drag" {
        vec![
            "action",
            "from_x",
            "from_y",
            "to_x",
            "to_y",
            "via",
            "steps",
            "duration_ms",
        ]
    } else {
        return Err("invalid_hold_action");
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("invalid_hold_action");
    }
    if kind == "click" {
        let point = (coordinate(action.get("x"))?, coordinate(action.get("y"))?);
        return Ok((vec![point, point], 50));
    }
    let from = (
        coordinate(action.get("from_x"))?,
        coordinate(action.get("from_y"))?,
    );
    let to = (
        coordinate(action.get("to_x"))?,
        coordinate(action.get("to_y"))?,
    );
    let duration = match action.get("duration_ms") {
        None => 500,
        Some(v) => v
            .as_u64()
            .filter(|v| *v <= 10000)
            .ok_or("invalid_hold_duration")?,
    };
    let mut points = vec![from];
    if let Some(via) = action.get("via") {
        if action.get("steps").is_some() {
            return Err("conflicting_hold_fields");
        }
        let via = via
            .as_array()
            .filter(|v| (1..=254).contains(&v.len()))
            .ok_or("invalid_hold_path")?;
        for point in via {
            if point.as_object().is_none_or(|p| p.len() != 2) {
                return Err("invalid_hold_point");
            }
            points.push((coordinate(point.get("x"))?, coordinate(point.get("y"))?));
        }
    } else {
        let steps = match action.get("steps") {
            None => 20,
            Some(v) => v
                .as_u64()
                .filter(|v| (1..=200).contains(v))
                .ok_or("invalid_hold_steps")?,
        };
        for i in 1..steps {
            let fraction = i as f64 / steps as f64;
            points.push((
                from.0 + (to.0 - from.0) * fraction,
                from.1 + (to.1 - from.1) * fraction,
            ));
        }
    }
    points.push(to);
    Ok((points, duration))
}
impl Request {
    /// Validate the entire command before native activation, key-down or cursor movement.
    pub fn parse(args: &Value) -> Result<Self, &'static str> {
        let object = args.as_object().ok_or("invalid_hold_request")?;
        if object.keys().any(|key| {
            ![
                "session",
                "_session_id",
                "_transport_session_id",
                "_public_session_label",
                "_session_idle_ttl_ms",
                "cursor_id",
                "pid",
                "window_id",
                "scope",
                "keys",
                "duration_ms",
                "actions",
                "delivery_mode",
            ]
            .contains(&key.as_str())
        }) || args
            .get("scope")
            .is_some_and(|v| v.as_str() != Some("window"))
        {
            return Err("invalid_hold_request");
        }
        let pid = args["pid"]
            .as_u64()
            .filter(|v| *v > 0 && *v <= u32::MAX as u64)
            .ok_or("invalid_hold_target")? as u32;
        let window_id = args["window_id"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("invalid_hold_target")?;
        let raw_keys = args["keys"]
            .as_array()
            .filter(|v| (1..=8).contains(&v.len()))
            .ok_or("invalid_hold_keys")?;
        let mut keys = Vec::new();
        for key in raw_keys {
            let key = key
                .as_str()
                .filter(|k| valid_key(k))
                .ok_or("invalid_hold_keys")?;
            if keys.iter().any(|existing| existing == key) {
                return Err("invalid_hold_keys");
            }
            keys.push(key.to_string());
        }
        keys.sort_by_key(|key| !modifier(key));
        let duration_ms = args["duration_ms"]
            .as_u64()
            .filter(|v| (1..=10000).contains(v))
            .ok_or("invalid_hold_duration")?;
        let actions = match args.get("actions") {
            None => vec![],
            Some(v) => v
                .as_array()
                .filter(|v| (1..=32).contains(&v.len()))
                .ok_or("invalid_hold_actions")?
                .clone(),
        };
        let mut requested = 0;
        for action in &actions {
            requested += points(action)?.1;
        }
        if requested > duration_ms {
            return Err("hold_duration_exceeded");
        }
        Ok(Self {
            pid,
            window_id,
            keys,
            duration_ms,
            actions,
        })
    }
    pub fn into_hold(self, ratio: f64) -> Result<Hold, &'static str> {
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("invalid_hold_scale");
        }
        let mut actions = Vec::new();
        for action in &self.actions {
            let (points, duration) = points(action)?;
            let points = points
                .into_iter()
                .map(|(x, y)| {
                    let (x, y) = ((x * ratio).round(), (y * ratio).round());
                    if !x.is_finite()
                        || !y.is_finite()
                        || x > i32::MAX as f64
                        || y > i32::MAX as f64
                    {
                        return Err("invalid_hold_point");
                    }
                    Ok(Point {
                        x: x as i32,
                        y: y as i32,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            actions.push(Path::new(points, duration)?);
        }
        Ok(Hold {
            keys: self.keys,
            duration_ms: self.duration_ms,
            actions,
        })
    }
}
#[cfg(test)]
mod tests;
