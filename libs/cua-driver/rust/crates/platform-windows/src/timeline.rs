//! One timed input program: keys, every mouse button, absolute and relative
//! motion, wheel and pen contacts, released completely when it ends.
//!
//! The executor is pure so it can be tested without Windows; the native
//! backend lives in `input::timeline`.
use serde_json::{json, Value};

/// Longest accepted program and largest event count.
pub(crate) const MAX_DURATION_MS: u64 = 120_000;
pub(crate) const MAX_EVENTS: usize = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pen {
    /// 0..=1024, as Windows reports pen pressure.
    pub pressure: u32,
    /// -90..=90 degrees.
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub eraser: bool,
    pub barrel: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    KeyDown(String),
    KeyUp(String),
    ButtonDown(Button),
    ButtonUp(Button),
    /// Native window pixels, already scaled from the screenshot.
    Move {
        x: i32,
        y: i32,
    },
    /// Raw relative motion for pointer-locked apps; not scaled.
    MoveRelative {
        dx: i32,
        dy: i32,
    },
    /// Wheel detents; one detent is 120 native units.
    Wheel {
        dx: i32,
        dy: i32,
    },
    PenDown {
        x: i32,
        y: i32,
        pen: Pen,
    },
    PenMove {
        x: i32,
        y: i32,
        pen: Pen,
    },
    PenUp {
        x: i32,
        y: i32,
        pen: Pen,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Timed {
    pub at_ms: u64,
    pub event: Event,
}

pub(crate) struct Request {
    pub pid: u32,
    pub window_id: u64,
    /// Events with screenshot-pixel coordinates; `into_program` scales them.
    events: Vec<(u64, RawEvent)>,
}

#[derive(Clone, Debug)]
enum RawEvent {
    Plain(Event),
    Point {
        kind: &'static str,
        x: f64,
        y: f64,
        pen: Option<Pen>,
    },
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
}
fn integer(value: &Value, key: &str, min: i64, max: i64) -> Result<i64, &'static str> {
    let v = value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or("invalid_timeline_event")?;
    if v < min || v > max {
        return Err("invalid_timeline_event");
    }
    Ok(v)
}
fn button(value: &Value) -> Result<Button, &'static str> {
    match value
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or("left")
    {
        "left" => Ok(Button::Left),
        "right" => Ok(Button::Right),
        "middle" => Ok(Button::Middle),
        "x1" => Ok(Button::X1),
        "x2" => Ok(Button::X2),
        _ => Err("invalid_timeline_button"),
    }
}
fn pen(value: &Value) -> Result<Pen, &'static str> {
    let pressure = value.get("pressure").and_then(Value::as_f64).unwrap_or(0.5);
    if !(0.0..=1.0).contains(&pressure) {
        return Err("invalid_pen_pressure");
    }
    let tilt = |key| -> Result<i32, &'static str> {
        match value.get(key) {
            None => Ok(0),
            Some(v) => v
                .as_i64()
                .filter(|t| (-90..=90).contains(t))
                .map(|t| t as i32)
                .ok_or("invalid_pen_tilt"),
        }
    };
    Ok(Pen {
        pressure: (pressure * 1024.0).round() as u32,
        tilt_x: tilt("tilt_x")?,
        tilt_y: tilt("tilt_y")?,
        eraser: value
            .get("eraser")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        barrel: value
            .get("barrel")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

impl Request {
    pub fn parse(args: &Value) -> Result<Self, &'static str> {
        let pid = args
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= u32::MAX as u64)
            .ok_or("invalid_target")? as u32;
        let window_id = args
            .get("window_id")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
            .ok_or("invalid_target")?;
        let raw = args
            .get("events")
            .and_then(Value::as_array)
            .ok_or("invalid_timeline")?;
        if raw.is_empty() || raw.len() > MAX_EVENTS {
            return Err("invalid_timeline_length");
        }
        let mut last = 0u64;
        let mut events = Vec::with_capacity(raw.len());
        for item in raw {
            let at = item
                .get("at_ms")
                .and_then(Value::as_u64)
                .ok_or("invalid_timeline_time")?;
            if at < last || at > MAX_DURATION_MS {
                return Err("invalid_timeline_time");
            }
            last = at;
            let kind = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or("invalid_timeline_event")?;
            let key = || {
                item.get("key")
                    .and_then(Value::as_str)
                    .filter(|k| !k.is_empty() && k.len() <= 32)
                    .map(str::to_owned)
                    .ok_or("invalid_timeline_key")
            };
            let event = match kind {
                "key_down" => RawEvent::Plain(Event::KeyDown(key()?)),
                "key_up" => RawEvent::Plain(Event::KeyUp(key()?)),
                "button_down" => RawEvent::Plain(Event::ButtonDown(button(item)?)),
                "button_up" => RawEvent::Plain(Event::ButtonUp(button(item)?)),
                "move_relative" => RawEvent::Plain(Event::MoveRelative {
                    dx: integer(item, "dx", -10_000, 10_000)? as i32,
                    dy: integer(item, "dy", -10_000, 10_000)? as i32,
                }),
                "wheel" => {
                    let dx = item
                        .get("dx")
                        .map(|_| integer(item, "dx", -50, 50))
                        .transpose()?
                        .unwrap_or(0) as i32;
                    let dy = item
                        .get("dy")
                        .map(|_| integer(item, "dy", -50, 50))
                        .transpose()?
                        .unwrap_or(0) as i32;
                    if dx == 0 && dy == 0 {
                        return Err("invalid_timeline_wheel");
                    }
                    RawEvent::Plain(Event::Wheel { dx, dy })
                }
                "move" | "pen_down" | "pen_move" | "pen_up" => {
                    let x = number(item, "x")
                        .filter(|v| *v >= 0.0)
                        .ok_or("invalid_timeline_point")?;
                    let y = number(item, "y")
                        .filter(|v| *v >= 0.0)
                        .ok_or("invalid_timeline_point")?;
                    let kind: &'static str = match kind {
                        "move" => "move",
                        "pen_down" => "pen_down",
                        "pen_move" => "pen_move",
                        _ => "pen_up",
                    };
                    RawEvent::Point {
                        kind,
                        x,
                        y,
                        pen: if kind == "move" {
                            None
                        } else {
                            Some(pen(item)?)
                        },
                    }
                }
                _ => return Err("invalid_timeline_event"),
            };
            events.push((at, event));
        }
        Ok(Self {
            pid,
            window_id,
            events,
        })
    }

    /// Scale screenshot pixels to native window pixels and check the program is well formed.
    pub fn into_program(self, ratio: f64) -> Result<Vec<Timed>, &'static str> {
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("invalid_timeline_scale");
        }
        let scale = |v: f64| -> Result<i32, &'static str> {
            let s = (v * ratio).round();
            if s > i32::MAX as f64 {
                Err("invalid_timeline_point")
            } else {
                Ok(s as i32)
            }
        };
        let mut program = Vec::with_capacity(self.events.len());
        for (at_ms, raw) in self.events {
            let event = match raw {
                RawEvent::Plain(event) => event,
                RawEvent::Point { kind, x, y, pen } => {
                    let (x, y) = (scale(x)?, scale(y)?);
                    match (kind, pen) {
                        ("move", _) => Event::Move { x, y },
                        ("pen_down", Some(pen)) => Event::PenDown { x, y, pen },
                        ("pen_move", Some(pen)) => Event::PenMove { x, y, pen },
                        (_, Some(pen)) => Event::PenUp { x, y, pen },
                        _ => return Err("invalid_timeline_event"),
                    }
                }
            };
            program.push(Timed { at_ms, event });
        }
        validate(&program)?;
        Ok(program)
    }
}

/// Reject programs that release something never pressed or press it twice.
fn validate(program: &[Timed]) -> Result<(), &'static str> {
    let mut keys: Vec<&str> = Vec::new();
    let mut buttons: Vec<Button> = Vec::new();
    let mut pen_down = false;
    for timed in program {
        match &timed.event {
            Event::KeyDown(k) => {
                if keys.contains(&k.as_str()) {
                    return Err("timeline_key_already_down");
                }
                keys.push(k);
            }
            Event::KeyUp(k) => {
                let i = keys
                    .iter()
                    .position(|x| x == k)
                    .ok_or("timeline_key_not_down")?;
                keys.remove(i);
            }
            Event::ButtonDown(b) => {
                if buttons.contains(b) {
                    return Err("timeline_button_already_down");
                }
                buttons.push(*b);
            }
            Event::ButtonUp(b) => {
                let i = buttons
                    .iter()
                    .position(|x| x == b)
                    .ok_or("timeline_button_not_down")?;
                buttons.remove(i);
            }
            Event::PenDown { .. } => {
                if pen_down {
                    return Err("timeline_pen_already_down");
                }
                pen_down = true;
            }
            Event::PenUp { .. } => {
                if !pen_down {
                    return Err("timeline_pen_not_down");
                }
                pen_down = false;
            }
            _ => {}
        }
        if keys.len() > 16 {
            return Err("timeline_too_many_keys");
        }
    }
    Ok(())
}

/// Native work; every method sends one event or checks the target.
pub(crate) trait Backend {
    fn check_target(&mut self) -> Result<(), String>;
    /// Sleep until `at_ms` after the start, checking the target meanwhile.
    fn wait_until(&mut self, at_ms: u64) -> Result<(), String>;
    fn send(&mut self, event: &Event) -> Result<(), String>;
    /// Release whatever this program still holds, in reverse order.
    fn release(
        &mut self,
        keys: &[String],
        buttons: &[Button],
        pen: Option<(i32, i32, Pen)>,
    ) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub total: usize,
    pub sent: usize,
    pub released: bool,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({"events_total": self.total, "events_sent": self.sent, "released": self.released,
            "error": self.error, "elapsed_ms": self.elapsed_ms})
    }
}

/// Run the program in order; on any error or completion release everything still held.
pub(crate) fn execute(program: &[Timed], backend: &mut impl Backend) -> Report {
    let mut report = Report {
        total: program.len(),
        ..Default::default()
    };
    let mut keys: Vec<String> = Vec::new();
    let mut buttons: Vec<Button> = Vec::new();
    let mut pen: Option<(i32, i32, Pen)> = None;
    for timed in program {
        let step = backend
            .wait_until(timed.at_ms)
            .and_then(|()| backend.check_target())
            .and_then(|()| backend.send(&timed.event));
        if let Err(error) = step {
            report.error = Some(error);
            break;
        }
        report.sent += 1;
        match &timed.event {
            Event::KeyDown(k) => keys.push(k.clone()),
            Event::KeyUp(k) => keys.retain(|x| x != k),
            Event::ButtonDown(b) => buttons.push(*b),
            Event::ButtonUp(b) => buttons.retain(|x| x != b),
            Event::PenDown { x, y, pen: p } | Event::PenMove { x, y, pen: p } => {
                if pen.is_some() || matches!(timed.event, Event::PenDown { .. }) {
                    pen = Some((*x, *y, *p));
                }
            }
            Event::PenUp { .. } => pen = None,
            _ => {}
        }
    }
    match backend.release(&keys, &buttons, pen) {
        Ok(()) => report.released = true,
        Err(error) => {
            report.released = false;
            report.error = Some(match report.error.take() {
                Some(first) => format!("{first}; {error}"),
                None => error,
            });
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        sent: Vec<Event>,
        fail_at: Option<usize>,
        released: Option<(Vec<String>, Vec<Button>, bool)>,
    }
    impl Backend for Fake {
        fn check_target(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn wait_until(&mut self, _at_ms: u64) -> Result<(), String> {
            Ok(())
        }
        fn send(&mut self, event: &Event) -> Result<(), String> {
            if self.fail_at == Some(self.sent.len()) {
                return Err("foreground_lost: test".into());
            }
            self.sent.push(event.clone());
            Ok(())
        }
        fn release(
            &mut self,
            keys: &[String],
            buttons: &[Button],
            pen: Option<(i32, i32, Pen)>,
        ) -> Result<(), String> {
            self.released = Some((keys.to_vec(), buttons.to_vec(), pen.is_some()));
            Ok(())
        }
    }
    fn program(events: Value) -> Result<Vec<Timed>, &'static str> {
        Request::parse(&json!({"pid": 1, "window_id": 2, "events": events}))?.into_program(2.0)
    }

    #[test]
    fn scales_points_but_not_relative_motion() {
        let p = program(json!([{"at_ms":0,"type":"move","x":10,"y":20},{"at_ms":5,"type":"move_relative","dx":3,"dy":-4}])).unwrap();
        assert_eq!(p[0].event, Event::Move { x: 20, y: 40 });
        assert_eq!(p[1].event, Event::MoveRelative { dx: 3, dy: -4 });
    }

    #[test]
    fn rejects_unbalanced_or_unordered_programs() {
        assert_eq!(
            program(json!([{"at_ms":0,"type":"key_up","key":"w"}])).unwrap_err(),
            "timeline_key_not_down"
        );
        assert_eq!(program(json!([{"at_ms":0,"type":"button_down","button":"middle"},{"at_ms":1,"type":"button_down","button":"middle"}])).unwrap_err(), "timeline_button_already_down");
        assert_eq!(program(json!([{"at_ms":5,"type":"key_down","key":"w"},{"at_ms":1,"type":"key_up","key":"w"}])).unwrap_err(), "invalid_timeline_time");
        assert_eq!(
            program(json!([{"at_ms":0,"type":"pen_down","x":1,"y":1,"pressure":2}])).unwrap_err(),
            "invalid_pen_pressure"
        );
        assert_eq!(
            program(json!([{"at_ms":0,"type":"wheel"}])).unwrap_err(),
            "invalid_timeline_wheel"
        );
    }

    #[test]
    fn releases_what_is_still_held_after_completion_or_failure() {
        let p = program(json!([
            {"at_ms":0,"type":"key_down","key":"shift"},
            {"at_ms":0,"type":"button_down","button":"middle"},
            {"at_ms":10,"type":"move","x":5,"y":5},
            {"at_ms":20,"type":"pen_down","x":5,"y":5,"pressure":0.3},
        ]))
        .unwrap();
        let mut fake = Fake::default();
        let report = execute(&p, &mut fake);
        assert_eq!(
            (report.sent, report.released, report.error.is_none()),
            (4, true, true)
        );
        assert_eq!(
            fake.released,
            Some((vec!["shift".to_string()], vec![Button::Middle], true))
        );

        let mut failing = Fake {
            fail_at: Some(2),
            ..Default::default()
        };
        let report = execute(&p, &mut failing);
        assert_eq!((report.sent, report.released), (2, true));
        assert!(report.error.unwrap().starts_with("foreground_lost"));
        assert_eq!(
            failing.released,
            Some((vec!["shift".to_string()], vec![Button::Middle], false))
        );
    }
}
