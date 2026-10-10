//! Exact-window native pointer motion. Delivery does not prove an app effect.

use crate::inputs::{element_token_schema, string_schema};
use crate::{
    InputDeliveryMode, Platform, SchemaMode, ToolAnnotations, ToolContract, ToolInput, ToolOutput,
};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Maximum requested dwell for one native pointer action.
pub const POINTER_MAX_DWELL_MS: u64 = 10_000;

fn background() -> InputDeliveryMode {
    InputDeliveryMode::Background
}
fn pid_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1,"maximum":2147483647})
}
fn window_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1})
}
fn coordinate_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"number","minimum":0})
}
fn capture_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"string","minLength":1})
}
fn dwell_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":0,"maximum":POINTER_MAX_DWELL_MS})
}

/// One capture-bound point or snapshot token belonging to an exact Windows window.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointerMoveInput {
    /// Process that owns the exact target window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact top-level native window identifier.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Current capture authority; required with x/y and forbidden with an element token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "capture_schema")]
    pub capture_id: Option<String>,
    /// Horizontal point in the admitted overview image, not desktop or detail pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "coordinate_schema")]
    pub x: Option<f64>,
    /// Vertical point in the same admitted overview image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "coordinate_schema")]
    pub y: Option<f64>,
    /// Current snapshot element token; cannot be combined with capture coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "element_token_schema")]
    pub element_token: Option<String>,
    /// Zero moves only; positive values request bounded dwell at the target.
    #[serde(default)]
    #[schemars(schema_with = "dwell_schema")]
    pub dwell_ms: u64,
    /// Background refuses. Explicit foreground leaves the target and pointer in place.
    #[serde(default = "background")]
    pub delivery_mode: InputDeliveryMode,
    /// Public session label, or omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}

impl ToolInput for PointerMoveInput {
    const TOOL_NAME: &'static str = "move_pointer";

    fn validate(&self) -> Result<(), String> {
        if self.pid == 0 || self.pid > i32::MAX as u32 || self.window_id == 0 {
            return Err("move_pointer requires a positive pid and exact window_id".into());
        }
        let capture = self
            .capture_id
            .as_ref()
            .is_some_and(|id| !id.trim().is_empty());
        let element = self
            .element_token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty());
        let capture_target =
            capture && self.x.is_some() && self.y.is_some() && self.element_token.is_none();
        let element_target =
            element && self.capture_id.is_none() && self.x.is_none() && self.y.is_none();
        if !capture_target && !element_target {
            return Err("move_pointer requires exactly one capture point or element token".into());
        }
        if [self.x, self.y]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("move_pointer coordinates must be finite and nonnegative".into());
        }
        if self.dwell_ms > POINTER_MAX_DWELL_MS {
            return Err("move_pointer dwell_ms must be at most 10000".into());
        }
        Ok(())
    }
}

/// Only native motion is a successful route for this tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerMoveRoute {
    Native,
}

/// Native delivery never claims a verified tooltip or other application effect.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerMoveEffect {
    Unverifiable,
}

/// The first Windows implementation supports only explicit foreground delivery.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointerMoveDelivery {
    Foreground,
}

/// Successful native delivery plus measured effects; null means measurement unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointerMoveOutput {
    /// Whether the native motion call was entered.
    pub attempted: bool,
    /// Whether the native backend accepted its one movement event.
    pub delivered: bool,
    /// Native OS input, never an overlay or posted-message substitute.
    pub route: PointerMoveRoute,
    /// Application effects still need an independent observation.
    pub effect: PointerMoveEffect,
    /// Explicit foreground delivery; automatic restoration is not performed.
    pub delivery_mode: PointerMoveDelivery,
    /// Requested bounded dwell; zero denotes motion only.
    pub dwell_ms: u64,
    /// Actual monotonic elapsed time after movement, including scheduler delay.
    pub dwell_elapsed_ms: u64,
    /// Comparison of foreground before the operation and at receipt measurement.
    pub foreground_changed: Option<bool>,
    /// Comparison of pointer position before the operation and at receipt measurement.
    pub pointer_changed: Option<bool>,
    /// Whether the exact target owns foreground at receipt measurement.
    pub foreground_at_target: Option<bool>,
    /// Whether the pointer remains at its confirmed window-owned native destination.
    pub pointer_at_target: Option<bool>,
}

impl ToolOutput for PointerMoveOutput {
    fn validate(&self) -> Result<(), String> {
        if !self.attempted || !self.delivered {
            return Err(
                "move_pointer success requires attempted and delivered native motion".into(),
            );
        }
        if self.dwell_ms > POINTER_MAX_DWELL_MS {
            return Err("move_pointer dwell_ms must be at most 10000".into());
        }
        if self.dwell_elapsed_ms < self.dwell_ms {
            return Err("move_pointer success requires the requested dwell to complete".into());
        }
        Ok(())
    }
}

pub(crate) fn contracts() -> Vec<ToolContract> {
    let mut input_schema = PointerMoveInput::input_schema();
    input_schema["oneOf"] = json!([
        {"required":["capture_id","x","y"],"not":{"required":["element_token"]}},
        {"required":["element_token"],"not":{"anyOf":[
            {"required":["capture_id"]},{"required":["x"]},{"required":["y"]}
        ]}}
    ]);
    vec![ToolContract {
        name: PointerMoveInput::TOOL_NAME.into(),
        description: "Move the real pointer once against an exact admitted Windows window, with optional bounded hover dwell. No button or key events. Background refuses; explicit foreground leaves the target active and pointer in place. Completion is native delivery, not proof of an application hover effect; observe again.".into(),
        platforms: vec![Platform::Windows],
        aliases: vec![],
        capabilities: vec!["input.pointer.move".into(), "input.delivery_mode".into(), "accessibility.element_tokens".into()],
        annotations: ToolAnnotations { read_only: false, destructive: true, idempotent: false, open_world: true },
        schema_mode: SchemaMode::CanonicalRuntime,
        cursor_semantics: None,
        input_schema,
        success_output_schema: Some(PointerMoveOutput::output_schema()),
        error_output_schema: None,
        output_validator: crate::validate_typed_output::<PointerMoveOutput>,
    }]
}
