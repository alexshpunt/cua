//! Typed presentation requests. Presentation never grants capture or input authority.
use crate::{ToolInput, VisualPreset, VisualQuality};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_GENERATION: u64 = (1u64 << 53) - 1;

/// An omitted setting keeps its current value, rather than resetting the session.
#[derive(Clone, Default, Serialize, Deserialize, JsonSchema, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct SetAgentVisualsInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<VisualPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observations: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previews: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_drag: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<VisualQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduced_motion: Option<bool>,
}
impl ToolInput for SetAgentVisualsInput {
    const TOOL_NAME: &'static str = "set_agent_visuals";
    fn input_schema() -> Value {
        visual_schema(Self::TOOL_NAME)
    }
    fn validate(&self) -> Result<(), String> {
        if self
            .opacity
            .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        {
            return Err("invalid_visual_opacity".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema, uniffi::Enum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    Overview,
    Detail,
    Inspect,
    Verify,
    Action,
    Clear,
}
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema, uniffi::Enum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityOutcome {
    Delivered,
    Refused,
    Unknown,
    Verified,
}
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema, uniffi::Enum)]
pub enum AgentActivityImageMime {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
}
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct AgentActivityRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl AgentActivityRect {
    fn valid(self) -> bool {
        self.x.unsigned_abs() <= 2_000_000
            && self.y.unsigned_abs() <= 2_000_000
            && (1..=32768).contains(&self.width)
            && (1..=32768).contains(&self.height)
    }
    fn contains(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && other.x >= self.x
            && other.y >= self.y
            && i64::from(other.x) + i64::from(other.width)
                <= i64::from(self.x) + i64::from(self.width)
            && i64::from(other.y) + i64::from(other.height)
                <= i64::from(self.y) + i64::from(self.height)
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct AgentActivityPoint {
    pub x: i32,
    pub y: i32,
}
/// The variants prevent mixing a display source with window ownership evidence.
#[derive(Clone, Serialize, Deserialize, JsonSchema, uniffi::Enum)]
#[serde(untagged, deny_unknown_fields)]
pub enum AgentActivitySource {
    Window {
        window_id: u64,
        pid: u32,
        identity: String,
        bounds: AgentActivityRect,
    },
    Display {
        display: String,
        bounds: AgentActivityRect,
    },
}
impl AgentActivitySource {
    fn bounds(&self) -> AgentActivityRect {
        match self {
            Self::Window { bounds, .. } | Self::Display { bounds, .. } => *bounds,
        }
    }
    fn valid(&self) -> bool {
        if !self.bounds().valid() {
            return false;
        }
        match self {
            Self::Window {
                window_id,
                pid,
                identity,
                ..
            } => {
                if *window_id == 0
                    || *window_id > MAX_GENERATION
                    || *pid == 0
                    || identity.len() > 128
                {
                    return false;
                }
                let parts: Vec<_> = identity.split(':').collect();
                if parts.len() != 3 || parts[1] != pid.to_string() || parts[2].starts_with('0') {
                    return false;
                }
                let Some(property) = parts[0].strip_prefix("PiComputerUse.") else {
                    return false;
                };
                property.len() == 32
                    && property.bytes().all(|c| c.is_ascii_hexdigit())
                    && parts[2].parse::<u64>().is_ok_and(|v| v > 0)
            }
            Self::Display { display, .. } => {
                !display.is_empty()
                    && display.len() <= 256
                    && !display.chars().any(char::is_control)
            }
        }
    }
}
/// No Debug implementation: image bytes must not enter diagnostics.
#[derive(Clone, Serialize, Deserialize, JsonSchema, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct AgentActivityImage {
    pub data: String,
    pub mime_type: AgentActivityImageMime,
}
/// One bounded publication of already-returned pixels; this request captures nothing.
#[derive(Clone, Serialize, Deserialize, JsonSchema, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct PresentAgentActivityInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub generation: u64,
    pub kind: AgentActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentActivitySource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<AgentActivityRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<AgentActivityPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<AgentActivityImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentActivityOutcome>,
}
impl ToolInput for PresentAgentActivityInput {
    const TOOL_NAME: &'static str = "present_agent_activity";
    fn input_schema() -> Value {
        visual_schema(Self::TOOL_NAME)
    }
    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_GENERATION).contains(&self.generation) {
            return Err("invalid_generation".into());
        }
        if self.kind == AgentActivityKind::Clear {
            return if self.source.is_none()
                && self.image.is_none()
                && self.region.is_none()
                && self.point.is_none()
            {
                Ok(())
            } else {
                Err("invalid_clear".into())
            };
        }
        let source = self.source.as_ref().ok_or("source_required")?;
        if !source.valid() {
            return Err("invalid_source".into());
        }
        let bounds = source.bounds();
        if self.region.is_some_and(|r| !bounds.contains(r)) {
            return Err("region_outside_source".into());
        }
        if self.point.is_some_and(|p| {
            !bounds.contains(AgentActivityRect {
                x: p.x,
                y: p.y,
                width: 1,
                height: 1,
            })
        }) {
            return Err("point_outside_source".into());
        }
        if let Some(image) = &self.image {
            if image.data.is_empty() || image.data.len() > 12 * 1024 * 1024 {
                return Err("invalid_preview".into());
            }
            if self.kind == AgentActivityKind::Action
                || matches!(
                    self.outcome,
                    Some(AgentActivityOutcome::Refused | AgentActivityOutcome::Unknown)
                )
            {
                return Err("preview_without_observation".into());
            }
        }
        if self.action.as_ref().is_some_and(|s| {
            s.len() > 64 || !s.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
        }) {
            return Err("invalid_action".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observed_window() -> Value {
        json!({"generation":2,"kind":"overview","source":{
            "window_id":73,"pid":42,"identity":"PiComputerUse.00000000000000000000000000000001:42:1",
            "bounds":{"x":-20,"y":10,"width":80,"height":40}},
            "region":{"x":-10,"y":12,"width":10,"height":8},
            "point":{"x":-9,"y":13},"image":{"data":"YWJj","mime_type":"image/jpeg"},
            "action":"observe","outcome":"verified"})
    }
    #[test]
    fn typed_window_publication_round_trips_the_existing_wire_shape() {
        let value = observed_window();
        let input: PresentAgentActivityInput = serde_json::from_value(value.clone()).unwrap();
        assert!(input.validate().is_ok());
        assert_eq!(serde_json::to_value(input).unwrap(), value);
    }
    #[test]
    fn invalid_geometry_and_window_ownership_are_not_admitted() {
        for identity in [
            "PiComputerUse.00000000000000000000000000000001:41:1",
            "PiComputerUse.00000000000000000000000000000001:42:01",
            "PiComputerUse.invalid:42:1",
        ] {
            let mut value = observed_window();
            value["source"]["identity"] = json!(identity);
            assert!(serde_json::from_value::<PresentAgentActivityInput>(value)
                .unwrap()
                .validate()
                .is_err());
        }
        for (field, value) in [
            ("generation", json!(0)),
            ("generation", json!(9007199254740992u64)),
            ("region", json!({"x":60,"y":10,"width":1,"height":1})),
            ("point", json!({"x":60,"y":10})),
            ("action", json!("unsafe action")),
            ("outcome", json!("unknown")),
        ] {
            let mut input = observed_window();
            input[field] = value;
            assert!(serde_json::from_value::<PresentAgentActivityInput>(input)
                .unwrap()
                .validate()
                .is_err());
        }
    }
    #[test]
    fn partial_configuration_rejects_nonfinite_and_out_of_range_opacity() {
        for opacity in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(SetAgentVisualsInput {
                opacity: Some(opacity),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        for opacity in [0.0, 0.5, 1.0] {
            assert!(SetAgentVisualsInput {
                opacity: Some(opacity),
                ..Default::default()
            }
            .validate()
            .is_ok());
        }
    }
}

fn visual_schema(name: &str) -> Value {
    super::agent_visuals::contracts()
        .into_iter()
        .find(|tool| tool.name == name)
        .expect("visual tool has a canonical contract")
        .input_schema
}
