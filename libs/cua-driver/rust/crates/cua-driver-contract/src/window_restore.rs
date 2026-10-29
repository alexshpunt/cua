//! Exact-window state and explicit restoration. Neither operation injects input.

use crate::inputs::string_schema;
use crate::{Platform, SchemaMode, ToolAnnotations, ToolContract, ToolInput, ToolOutput};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

fn pid_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1,"maximum":2147483647})
}
fn window_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1})
}
fn state_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"string","minLength":1})
}

fn validate_identity(tool: &str, pid: u32, window_id: u64) -> Result<(), String> {
    if pid == 0 || pid > i32::MAX as u32 || window_id == 0 {
        return Err(format!(
            "{tool} requires a positive pid and exact window_id"
        ));
    }
    Ok(())
}

fn validate_state(tool: &str, pid: u32, window_id: u64, state_id: &str) -> Result<(), String> {
    validate_identity(tool, pid, window_id)?;
    if state_id.trim().is_empty() {
        return Err(format!("{tool} requires a nonblank state_id"));
    }
    Ok(())
}

/// Read native state without pixels, a UI tree, activation or input.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[derive(uniffi::Record)]
pub struct WindowStatusInput {
    /// Process that owns the exact target window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact top-level native window identifier.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// For multi-call work, prefer a short public session label and repeat it on every call that accepts it.
    /// Omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}
impl ToolInput for WindowStatusInput {
    const TOOL_NAME: &'static str = "get_window_status";
    fn validate(&self) -> Result<(), String> {
        validate_identity(Self::TOOL_NAME, self.pid, self.window_id)
    }
}

/// One fresh, narrowly scoped state reference; no screenshot coordinates are accepted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[derive(uniffi::Record)]
pub struct WindowRestoreInput {
    /// Process that owns the exact target window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact top-level native window identifier.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Fresh one-use native window-state admission, not a capture or element token.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// For multi-call work, prefer a short public session label and repeat it on every call that accepts it.
    /// Omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}
impl ToolInput for WindowRestoreInput {
    const TOOL_NAME: &'static str = "restore_window";
    fn validate(&self) -> Result<(), String> {
        validate_state(Self::TOOL_NAME, self.pid, self.window_id, &self.state_id)
    }
}

/// Native state plus one-use restoration authority; focus is null when unmeasurable.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowStatusOutput {
    /// Process that owns the sampled target.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact sampled native window.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// One-use state admission; it grants neither pixel nor typing authority.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// Whether the native window is minimized at sampling time.
    pub minimized: bool,
    /// Whether the native window is visible at sampling time, not a fresh-paint claim.
    pub visible: bool,
    /// Whether this exact target is currently foreground.
    pub foreground_at_target: bool,
    /// Whether native keyboard focus belongs to this target; null means unavailable.
    pub keyboard_focus_at_target: Option<bool>,
}
impl ToolOutput for WindowStatusOutput {
    fn validate(&self) -> Result<(), String> {
        validate_state(
            WindowStatusInput::TOOL_NAME,
            self.pid,
            self.window_id,
            &self.state_id,
        )
    }
}

/// Native window management, never an input-event workaround.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowRestoreRoute {
    Native,
}

/// Confirms only measured native window state, never fresh application pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowRestoreEffect {
    Confirmed,
}

/// Successful restoration and activation with independently measured native effects.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowRestoreOutput {
    /// Process that owns the admitted target.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact admitted native window.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Consumed state identity, echoed for receipt matching.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// The restore method was entered, including an uncertain request.
    pub restore_attempted: bool,
    /// The native backend accepted the single restore request.
    pub restore_requested: bool,
    /// The target was independently observed no longer minimized.
    pub restored: bool,
    /// The non-input foreground activation method was entered.
    pub activation_attempted: bool,
    /// Activation completed with the exact target observed foreground.
    pub activation_succeeded: bool,
    /// Final native visibility; null means unavailable, not fresh paint.
    pub visible: Option<bool>,
    /// Final exact-target foreground membership; null means unavailable.
    pub foreground_at_target: Option<bool>,
    /// Final keyboard focus belongs to the target; null means unavailable.
    pub keyboard_focus_at_target: Option<bool>,
    /// Foreground changed from before the request; null means unavailable.
    pub foreground_changed: Option<bool>,
    /// Native window management, without injected keyboard or pointer input.
    pub route: WindowRestoreRoute,
    /// Confirms native state only; read the window again for pixels.
    pub effect: WindowRestoreEffect,
}
impl ToolOutput for WindowRestoreOutput {
    fn validate(&self) -> Result<(), String> {
        validate_state(
            WindowRestoreInput::TOOL_NAME,
            self.pid,
            self.window_id,
            &self.state_id,
        )?;
        if !self.restore_attempted
            || !self.restore_requested
            || !self.restored
            || !self.activation_attempted
            || !self.activation_succeeded
            || self.visible != Some(true)
            || self.foreground_at_target != Some(true)
        {
            return Err("restore_window success requires requested and observed restoration, activation and visible target foreground".into());
        }
        Ok(())
    }
}

pub(crate) fn contracts() -> Vec<ToolContract> {
    vec![
        ToolContract {
            name: WindowStatusInput::TOOL_NAME.into(),
            description: "Use get_window_status to read minimized, visible, foreground and separately measured keyboard-focus state of one exact Windows window without capturing pixels, walking a UI tree or changing activation. The returned state reference grants one-use restore authority, not pixel or typing authority.".into(),
            platforms: vec![Platform::Windows], aliases: vec![], capabilities: vec!["window.state".into()],
            annotations: ToolAnnotations { read_only: true, destructive: false, idempotent: false, open_world: true },
            schema_mode: SchemaMode::CanonicalRuntime, cursor_semantics: None,
            input_schema: WindowStatusInput::input_schema(), success_output_schema: Some(WindowStatusOutput::output_schema()),
            error_output_schema: None, output_validator: crate::validate_typed_output::<WindowStatusOutput>,
        },
        ToolContract {
            name: WindowRestoreInput::TOOL_NAME.into(),
            description: "Use restore_window to restore and explicitly activate one exact Windows window. Native restoration and activation inject no keyboard or pointer input. Success leaves the target active. Do not replay an interrupted or uncertain request. Read the target again for pixels; native state confirmation does not establish fresh paint.".into(),
            platforms: vec![Platform::Windows], aliases: vec![], capabilities: vec!["window.restore".into()],
            annotations: ToolAnnotations { read_only: false, destructive: true, idempotent: false, open_world: true },
            schema_mode: SchemaMode::CanonicalRuntime, cursor_semantics: None,
            input_schema: WindowRestoreInput::input_schema(), success_output_schema: Some(WindowRestoreOutput::output_schema()),
            error_output_schema: None, output_validator: crate::validate_typed_output::<WindowRestoreOutput>,
        },
    ]
}
