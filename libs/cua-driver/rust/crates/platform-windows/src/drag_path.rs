//! A bounded held path. The backend owns native targeting, timing and input.

/// Native local pixels measured from the captured window's top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Point {
    pub x: i32,
    pub y: i32,
}

pub(crate) struct Path {
    pub points: Vec<Point>,
    pub duration_ms: u64,
}

impl Path {
    pub fn new(points: Vec<Point>, duration_ms: u64) -> Result<Self, &'static str> {
        if !(2..=256).contains(&points.len()) {
            return Err("invalid_path_length");
        }
        if points.iter().any(|point| point.x < 0 || point.y < 0) {
            return Err("invalid_path_point");
        }
        if duration_ms > 10_000 {
            return Err("invalid_path_duration");
        }
        Ok(Self {
            points,
            duration_ms,
        })
    }
}

/// Every successful press owns one release, including a handled path failure.
pub(crate) trait Backend {
    fn check_target(&mut self) -> Result<(), String>;
    fn move_to(&mut self, point: Point) -> Result<(), String>;
    fn press(&mut self) -> Result<(), String>;
    fn release(&mut self) -> Result<(), String>;
    fn wait_until(&mut self, elapsed_ms: u64) -> Result<(), String>;
}

#[derive(Debug)]
pub(crate) struct Report {
    pub completed_points: usize,
    pub pressed: bool,
    pub released: bool,
    pub error: Option<String>,
}

impl Report {
    /// Keep the point prefix in the driver's closed public delivery contract.
    /// The summary carries press/release receipts, not proof of an app result.
    pub fn into_tool_result(
        self,
        total: usize,
        duration_ms: u64,
        elapsed_ms: u64,
    ) -> cua_driver_core::protocol::ToolResult {
        use cua_driver_core::action_record::{
            ActionEffect, ActionExecutionRecord, ActionRefusal, ActionTransport, ActualDelivery,
            RequestedDelivery,
        };
        use cua_driver_core::protocol::ToolResult;
        let effect = if self.error.is_none() {
            ActionEffect::Unverifiable
        } else if self.completed_points == 0 {
            ActionEffect::Refused
        } else {
            ActionEffect::Partial
        };
        let mut record = ActionExecutionRecord::new(
            effect,
            ActionTransport::WindowsSendInput,
            RequestedDelivery::Foreground,
        );
        if self.completed_points > 0 {
            record.actual_delivery = Some(ActualDelivery::Foreground);
            record.delivered_count = Some(self.completed_points as u32);
        } else if let Some(error) = self.error.as_deref() {
            record.refusal = Some(ActionRefusal {
                code: error.split(':').next().unwrap_or("input_failed").into(),
                hint: Some(
                    "Observe the target before another attempt. Do not replay an uncertain path."
                        .into(),
                ),
            });
        }
        let message = format!("Windows held path: {}/{total} points, pressed={}, released={}, duration_ms={duration_ms}, elapsed_ms={elapsed_ms}. {}",
            self.completed_points, self.pressed, self.released,
            self.error.as_deref().unwrap_or("Delivery is not proof of the app's result."));
        // Handled partial delivery is an action outcome. The MCP error arm
        // skips action publication and would discard its completed prefix.
        if self.completed_points == 0 {
            if let Some(error) = self.error.as_deref() {
                return ToolResult::error(message).with_structured(serde_json::json!({
                    "code": error.split(':').next().unwrap_or("input_failed"),
                    "effect": "refused"
                }));
            }
        }
        ToolResult::text(message).with_action_record(record)
    }
}
struct HeldButton<'a, B: Backend> {
    backend: &'a mut B,
    released: bool,
}

impl<B: Backend> Drop for HeldButton<'_, B> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.backend.release();
        }
    }
}

/// Execute one validated path; successful moves form the reported prefix.
pub(crate) fn execute(path: &Path, backend: &mut impl Backend) -> Report {
    let mut report = Report {
        completed_points: 0,
        pressed: false,
        released: false,
        error: None,
    };
    if let Err(error) = backend
        .check_target()
        .and_then(|()| backend.move_to(path.points[0]))
    {
        report.error = Some(error);
        return report;
    }
    report.completed_points = 1;
    if let Err(error) = backend.check_target().and_then(|()| backend.press()) {
        report.error = Some(error);
        return report;
    }
    report.pressed = true;
    let mut held = HeldButton {
        backend,
        released: false,
    };
    for (index, point) in path.points.iter().enumerate().skip(1) {
        let deadline = path.duration_ms * index as u64 / (path.points.len() - 1) as u64;
        if let Err(error) = held
            .backend
            .wait_until(deadline)
            .and_then(|()| held.backend.check_target())
            .and_then(|()| held.backend.move_to(*point))
        {
            report.error = Some(error);
            break;
        }
        report.completed_points += 1;
    }
    match held.backend.release() {
        Ok(()) => {
            held.released = true;
            report.released = true;
        }
        Err(error) => {
            report.error = Some(match report.error.take() {
                Some(prior) => format!("{prior}; {error}"),
                None => error,
            });
        }
    }
    report
}

/// A screenshot-space request, validated before native targeting or activation.
pub(crate) struct Request {
    pub pid: u32,
    pub window_id: u64,
    pub points: Vec<(f64, f64)>,
    pub duration_ms: u64,
}

impl Request {
    pub fn parse(args: &serde_json::Value) -> Result<Self, &'static str> {
        use serde_json::Value;
        let pid = args
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
            .ok_or("invalid_path_target")? as u32;
        let window_id = args
            .get("window_id")
            .and_then(Value::as_u64)
            .filter(|hwnd| *hwnd > 0)
            .ok_or("invalid_path_target")?;
        if args
            .get("scope")
            .is_some_and(|value| value.as_str() != Some("window"))
        {
            return Err("invalid_path_scope");
        }
        if args.get("steps").is_some()
            || args.get("modifier").is_some()
            || args
                .get("from_zoom")
                .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err("conflicting_path_fields");
        }
        if args
            .get("button")
            .is_some_and(|value| value.as_str() != Some("left"))
        {
            return Err("invalid_path_button");
        }
        let via = args
            .get("via")
            .and_then(Value::as_array)
            .ok_or("invalid_path_length")?;
        if !(1..=254).contains(&via.len()) {
            return Err("invalid_path_length");
        }
        let coordinate = |value: Option<&Value>| {
            value
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or("invalid_path_point")
        };
        let mut points = Vec::with_capacity(via.len() + 2);
        points.push((
            coordinate(args.get("from_x"))?,
            coordinate(args.get("from_y"))?,
        ));
        for point in via {
            if point.as_object().is_none_or(|object| object.len() != 2) {
                return Err("invalid_path_point");
            }
            points.push((coordinate(point.get("x"))?, coordinate(point.get("y"))?));
        }
        points.push((coordinate(args.get("to_x"))?, coordinate(args.get("to_y"))?));
        let duration_ms = match args.get("duration_ms") {
            None => 500,
            Some(value) => value
                .as_u64()
                .filter(|ms| *ms <= 10_000)
                .ok_or("invalid_path_duration")?,
        };
        Ok(Self {
            pid,
            window_id,
            points,
            duration_ms,
        })
    }

    pub fn into_path(self, ratio: f64) -> Result<Path, &'static str> {
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("invalid_path_scale");
        }
        let scaled = |coordinate: f64| {
            let value = (coordinate * ratio).round();
            if !value.is_finite() || value > i32::MAX as f64 {
                Err("invalid_path_point")
            } else {
                Ok(value as i32)
            }
        };
        let points = self
            .points
            .into_iter()
            .map(|(x, y)| {
                Ok(Point {
                    x: scaled(x)?,
                    y: scaled(y)?,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        Path::new(points, self.duration_ms)
    }
}
#[cfg(test)]
mod tests;
