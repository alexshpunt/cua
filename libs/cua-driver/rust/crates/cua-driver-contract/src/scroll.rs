//! Capture-bound native wheel input. Wheel detents are not content pixels.

use crate::inputs::string_schema;
use crate::{
    InputDeliveryMode, Platform, PointerMoveDelivery, PointerMoveEffect, PointerMoveRoute,
    SchemaMode, ToolAnnotations, ToolContract, ToolInput, ToolOutput,
};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// Maximum absolute number of wheel detents per axis in one action.
pub const SCROLL_MAX_DETENTS: i32 = 50;

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
fn delta_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":-SCROLL_MAX_DETENTS,"maximum":SCROLL_MAX_DETENTS})
}

/// One detent is 120 native wheel-delta units, not a line or content pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScrollUnit {
    WheelDetents,
}

impl ScrollUnit {
    /// Validate both bounded axes before any native work is attempted.
    pub fn validate_deltas(self, delta_x: i32, delta_y: i32) -> Result<(), String> {
        if !(-SCROLL_MAX_DETENTS..=SCROLL_MAX_DETENTS).contains(&delta_x)
            || !(-SCROLL_MAX_DETENTS..=SCROLL_MAX_DETENTS).contains(&delta_y)
        {
            return Err("scroll_at deltas must be within -50..50 wheel detents".into());
        }
        if delta_x == 0 && delta_y == 0 {
            return Err("scroll_at requires a nonzero delta".into());
        }
        Ok(())
    }
}

/// Native wheel axis; nonzero vertical input is delivered before horizontal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScrollAxis {
    X,
    Y,
}

/// A current overview point belonging to one exact Windows window.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointerScrollInput {
    /// Process that owns the exact target window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact top-level native window identifier.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Use a current overview capture ID for this exact window, never a detail or display ID.
    #[schemars(schema_with = "capture_schema")]
    pub capture_id: String,
    /// Horizontal point in the admitted overview image, not desktop pixels.
    #[schemars(schema_with = "coordinate_schema")]
    pub x: f64,
    /// Vertical point in the same admitted overview image.
    #[schemars(schema_with = "coordinate_schema")]
    pub y: f64,
    /// Signed horizontal detents; positive scrolls content right. Defaults to zero.
    #[serde(default)]
    #[schemars(schema_with = "delta_schema")]
    pub delta_x: i32,
    /// Signed vertical detents; positive scrolls content down. Defaults to zero.
    #[serde(default)]
    #[schemars(schema_with = "delta_schema")]
    pub delta_y: i32,
    /// State wheel_detents explicitly; the application controls actual displacement.
    pub unit: ScrollUnit,
    /// Background refuses. Explicit foreground leaves the target and pointer active.
    #[serde(default = "background")]
    pub delivery_mode: InputDeliveryMode,
    /// For multi-call work, prefer a short public session label and repeat it on every call that accepts it.
    /// Omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}

impl ToolInput for PointerScrollInput {
    const TOOL_NAME: &'static str = "scroll_at";

    fn validate(&self) -> Result<(), String> {
        if self.pid == 0 || self.pid > i32::MAX as u32 || self.window_id == 0 {
            return Err("scroll_at requires a positive pid and exact window_id".into());
        }
        if self.capture_id.trim().is_empty() {
            return Err("scroll_at requires a nonblank capture_id".into());
        }
        if [self.x, self.y]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("scroll_at coordinates must be finite and nonnegative".into());
        }
        self.unit.validate_deltas(self.delta_x, self.delta_y)
    }
}

/// Successful native input and measured effects, not verified content displacement.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointerScrollOutput {
    /// Whether native input was attempted.
    pub attempted: bool,
    /// Whether all requested wheel axes returned successful delivery.
    pub delivered: bool,
    /// Whether the one native pointer movement was attempted.
    pub pointer_attempted: bool,
    /// Whether the backend accepted that native movement.
    pub pointer_delivered: bool,
    /// Requested signed horizontal detents.
    pub delta_x: i32,
    /// Requested signed vertical detents.
    pub delta_y: i32,
    /// Wheel units; no content-distance conversion is reported.
    pub unit: ScrollUnit,
    /// Axes whose native methods were entered, in actual order.
    pub attempted_axes: Vec<ScrollAxis>,
    /// Axes whose native methods returned successful delivery, in actual order.
    pub delivered_axes: Vec<ScrollAxis>,
    /// Native OS input, never overlay or browser input.
    pub route: PointerMoveRoute,
    /// Application effects need an independent fresh observation.
    pub effect: PointerMoveEffect,
    /// Explicit foreground delivery without automatic restoration.
    pub delivery_mode: PointerMoveDelivery,
    /// Foreground comparison before input and at receipt time; null means unavailable.
    pub foreground_changed: Option<bool>,
    /// Pointer comparison before input and at receipt time; null means unavailable.
    pub pointer_changed: Option<bool>,
    /// Whether the target owns foreground at receipt time; null means unavailable.
    pub foreground_at_target: Option<bool>,
    /// Whether the pointer remains at its confirmed native target; null means unavailable.
    pub pointer_at_target: Option<bool>,
}

impl ToolOutput for PointerScrollOutput {
    fn validate(&self) -> Result<(), String> {
        if !self.attempted || !self.delivered || !self.pointer_attempted || !self.pointer_delivered
        {
            return Err(
                "scroll_at success requires attempted and delivered pointer and wheel input".into(),
            );
        }
        self.unit.validate_deltas(self.delta_x, self.delta_y)?;
        let expected: Vec<_> = [(ScrollAxis::Y, self.delta_y), (ScrollAxis::X, self.delta_x)]
            .into_iter()
            .filter_map(|(axis, delta)| (delta != 0).then_some(axis))
            .collect();
        if self.attempted_axes != expected || self.delivered_axes != expected {
            return Err("scroll_at success requires the complete requested axis sequence".into());
        }
        Ok(())
    }
}

pub(crate) fn contracts() -> Vec<ToolContract> {
    vec![ToolContract {
        name: PointerScrollInput::TOOL_NAME.into(),
        description: "Use scroll_at to deliver native wheel input at a current overview point in one exact Windows window without an accessibility scroll target. Delivery does not prove content displacement; observe the same window again.".into(),
        platforms: vec![Platform::Windows],
        aliases: vec![],
        capabilities: vec!["input.pointer.scroll".into(), "input.delivery_mode".into()],
        annotations: ToolAnnotations {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        },
        schema_mode: SchemaMode::CanonicalRuntime,
        cursor_semantics: None,
        input_schema: PointerScrollInput::input_schema(),
        success_output_schema: Some(PointerScrollOutput::output_schema()),
        error_output_schema: None,
        output_validator: crate::validate_typed_output::<PointerScrollOutput>,
    }]
}
