//! Windows-only read contracts for an exact native display. These pixels grant no input.

use crate::{
    CursorAction, CursorSemantics, Platform, SchemaMode, ToolAnnotations, ToolContract, ToolInput,
    ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
fn string_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    String::json_schema(g)
}
fn cap_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer","minimum":0,"maximum":4294967295_u64})
}

/// Host-global physical pixels; image-local pixels always start at zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DisplayBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// One GDI logical monitor. Identity is native but valid only with its topology token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DisplayInfo {
    pub display_id: String,
    pub monitor_handle: String,
    /// Native monitor interface names; mirrored devices may have more than one.
    pub monitor_device_paths: Vec<String>,
    pub bounds: DisplayBounds,
    pub work_area: DisplayBounds,
    pub primary: bool,
    /// GetScaleFactorForMonitor on success; never its documented failure fallback.
    pub scale_factor: Option<f64>,
    /// Unknown: do not derive DPI from scale or call a DPI-unaware API on a PMv2 thread.
    pub dpi: Option<u32>,
}

/// Enumerate connected displays without reading pixels.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDisplaysInput {
    /// For multi-call work, prefer a short public session label and repeat it on every call that
    /// accepts it. Omit it to use the authenticated transport's implicit lifecycle session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}
impl ToolInput for ListDisplaysInput {
    const TOOL_NAME: &'static str = "list_displays";
}

/// Read exactly the enumerated display in this topology; never fall back to primary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetDisplayStateInput {
    /// Exact native display_id returned by list_displays, not an enumeration index or primary alias.
    pub display_id: String,
    /// Exact topology_id from the same discovery; a changed topology refuses without pixels or fallback.
    pub topology_id: String,
    /// For multi-call work, prefer a short public session label and repeat it on every call that
    /// accepts it. Omit it to use the authenticated transport's implicit lifecycle session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
    /// Zero or omitted keeps native physical resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "cap_schema")]
    pub max_image_dimension: Option<u32>,
}
impl ToolInput for GetDisplayStateInput {
    const TOOL_NAME: &'static str = "get_display_state";
    fn validate(&self) -> Result<(), String> {
        if self.display_id.trim().is_empty() || self.topology_id.trim().is_empty() {
            return Err(
                "display_id and topology_id must be non-empty native discovery values".into(),
            );
        }
        Ok(())
    }
}

/// Current connected-monitor metadata and its exact read-geometry token.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListDisplaysOutput {
    pub platform: String,
    pub topology_id: String,
    pub displays: Vec<DisplayInfo>,
}
impl ToolOutput for ListDisplaysOutput {}

/// Selected-display pixels are local to display bounds, not desktop input coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DisplayStateOutput {
    pub platform: String,
    pub topology_id: String,
    pub display: DisplayInfo,
    pub screenshot_width: u32,
    pub screenshot_height: u32,
    pub screenshot_original_width: u32,
    pub screenshot_original_height: u32,
    pub screenshot_mime_type: String,
    pub agent_overlay_capture: crate::AgentOverlayCapture,
}
impl ToolOutput for DisplayStateOutput {}

pub(crate) fn contracts() -> Vec<ToolContract> {
    fn read<I: ToolInput, O: ToolOutput>(description: &str, capabilities: &[&str]) -> ToolContract {
        ToolContract {
            name: I::TOOL_NAME.into(),
            description: description.into(),
            platforms: vec![Platform::Windows],
            aliases: vec![],
            capabilities: capabilities.iter().map(|v| (*v).into()).collect(),
            annotations: ToolAnnotations {
                read_only: true,
                destructive: false,
                idempotent: false,
                open_world: false,
            },
            schema_mode: SchemaMode::CanonicalRuntime,
            cursor_semantics: Some(CursorSemantics::new(CursorAction::Observe)),
            input_schema: I::input_schema(),
            success_output_schema: Some(O::output_schema()),
            error_output_schema: None,
            output_validator: crate::validate_typed_output::<O>,
        }
    }
    vec![
        read::<ListDisplaysInput, ListDisplaysOutput>("List connected Windows displays with native physical bounds and topology. No screenshot or input authority.", &["screen.dimensions"]),
        read::<GetDisplayStateInput, DisplayStateOutput>("Read exactly one enumerated Windows display with matching topology. Pixels are display-local and do not authorize input; missing or changed targets refuse without primary fallback.", &["screen.capture", "screen.dimensions"]),
    ]
}
