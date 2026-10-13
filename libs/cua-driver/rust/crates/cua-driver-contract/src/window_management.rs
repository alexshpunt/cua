//! One-use exact-window management. Desktop-frame coordinates never grant pointer authority.

use crate::inputs::string_schema;
use crate::{
    Platform, SchemaMode, ToolAnnotations, ToolContract, ToolInput, ToolOutput,
    VirtualDesktopMembership,
};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn nullable_bool_schema(generator: &mut SchemaGenerator) -> Schema {
    Option::<bool>::json_schema(generator)
}
fn nullable_bounds_schema(generator: &mut SchemaGenerator) -> Schema {
    Option::<DesktopWindowBounds>::json_schema(generator)
}
fn nullable_sample_schema(generator: &mut SchemaGenerator) -> Schema {
    Option::<WindowControlSample>::json_schema(generator)
}
fn validate_identity(pid: u32, window_id: u64) -> Result<(), String> {
    if pid == 0 || pid > i32::MAX as u32 || window_id == 0 {
        return Err("Exact positive PID and window ID are required".into());
    }
    Ok(())
}

fn validate_state_id(state_id: &str) -> Result<(), String> {
    if state_id.trim().is_empty() {
        return Err("A nonblank management state ID is required".into());
    }
    Ok(())
}

fn validate_source_sample(sample: &WindowControlSample, window_id: u64) -> Result<(), String> {
    sample.validate()?;
    if sample.foreground_at_target != (sample.foreground_window == window_id) {
        return Err("Foreground measurement must match the exact window source".into());
    }
    Ok(())
}
fn pid_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1,"maximum":2147483647})
}
fn window_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1})
}
fn state_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"string","minLength":1})
}
fn position_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":["integer","null"],"minimum":-100000,"maximum":100000})
}
fn unit_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"string","enum":["windows_desktop_pixels"]})
}
fn size_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":["integer","null"],"minimum":1,"maximum":32767})
}

/// Explicit native desktop pixels, unrelated to overview or CSS coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowManagementSpace {
    WindowsDesktopPixels,
}

/// Native bounds source; a fallback is reported rather than silently equated to visible bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowBoundsSource {
    DwmVisibleFrame,
    WindowRect,
}

/// Physical desktop rectangle. GetWindowRect bounds may include invisible frame borders.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopWindowBounds {
    /// Left edge in physical Windows desktop pixels.
    pub x: i32,
    /// Top edge in physical Windows desktop pixels.
    pub y: i32,
    /// Positive width of the declared bounds source, including invisible borders only for window_rect.
    pub width: u32,
    /// Positive height of the declared bounds source.
    pub height: u32,
}
impl DesktopWindowBounds {
    /// Validate positive bounded dimensions and representable Win32 rectangle edges.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=32767).contains(&self.width) || !(1..=32767).contains(&self.height) {
            return Err("Window dimensions must be integers between 1 and 32767".into());
        }
        if self.x.checked_add(self.width as i32).is_none()
            || self.y.checked_add(self.height as i32).is_none()
        {
            return Err("Window rectangle edges must fit signed Win32 coordinates".into());
        }
        Ok(())
    }
}

/// Fresh readonly metadata, not paint or pixel-input authority.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowControlSample {
    /// Measured rectangle; interpret it with bounds_source, not a screenshot transform.
    pub bounds: DesktopWindowBounds,
    /// Actual DWM visible frame or explicit GetWindowRect fallback.
    pub bounds_source: WindowBoundsSource,
    /// Physical Windows desktop pixels, independent of overview size or CSS scale.
    pub coordinate_space: WindowManagementSpace,
    /// Whether the exact native window is iconic at sampling time.
    pub minimized: bool,
    /// Whether the exact native window is zoomed at sampling time.
    pub maximized: bool,
    /// Native visibility, not proof that pixels are on screen or freshly painted.
    pub visible: bool,
    /// Whether the independently measured foreground HWND equals the target.
    pub foreground_at_target: bool,
    /// Whether native keyboard focus belongs to the target; null means unavailable.
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_bool_schema")]
    pub keyboard_focus_at_target: Option<bool>,
    /// Independently measured foreground HWND; zero means no foreground window.
    pub foreground_window: u64,
    /// Independent virtual-desktop queries, retaining unavailable results and their errors.
    pub virtual_desktop: VirtualDesktopMembership,
}
impl WindowControlSample {
    /// Validate measurements without inventing focus or virtual-desktop membership.
    pub fn validate(&self) -> Result<(), String> {
        self.bounds.validate()?;
        self.virtual_desktop.validate()
    }
}

/// Read exact-window metadata and issue one management admission without capture or activation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowControlStateInput {
    /// Process that owns the exact sampled window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact top-level native window identifier; do not substitute another window of the process.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// For multi-call work, prefer a short public session label and repeat it on every call that accepts it.
    /// Omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}
impl ToolInput for WindowControlStateInput {
    const TOOL_NAME: &'static str = "get_window_control_state";
    fn validate(&self) -> Result<(), String> {
        validate_identity(self.pid, self.window_id)
    }
}

/// One native window mutation. Restore remains a separate existing operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowManagementOperation {
    Move,
    Resize,
    Minimize,
    Maximize,
    Focus,
}

/// A source-bound one-use operation, with no input-event, delivery-mode or sequence fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowManagementInput {
    /// Process that owns the admitted exact window.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact native window from the sampled control state.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Submit the fresh one-use get_window_control_state reference, not a screenshot or element token.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// Choose one native operation. Focus never restores a minimized window; maximize may activate it.
    pub operation: WindowManagementOperation,
    /// For move, supply the left desktop-pixel position between -100000 and 100000. Omit for other operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "position_schema")]
    pub x: Option<i32>,
    /// For move, supply the top desktop-pixel position between -100000 and 100000. Omit for other operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "position_schema")]
    pub y: Option<i32>,
    /// For resize, supply the width between 1 and 32767 desktop pixels. Omit for other operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "size_schema")]
    pub width: Option<u32>,
    /// For resize, supply the height between 1 and 32767 desktop pixels. Omit for other operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "size_schema")]
    pub height: Option<u32>,
    /// For move or resize, supply windows_desktop_pixels and use the state's declared bounds source. Omit otherwise; overview and CSS coordinates are not accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "unit_schema")]
    pub unit: Option<WindowManagementSpace>,
    /// For multi-call work, prefer a short public session label and repeat it on every call that accepts it.
    /// Omit for the authenticated transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "string_schema")]
    pub session: Option<String>,
}
impl ToolInput for WindowManagementInput {
    const TOOL_NAME: &'static str = "manage_window";
    fn validate(&self) -> Result<(), String> {
        validate_identity(self.pid, self.window_id)?;
        validate_state_id(&self.state_id)?;
        match self.operation {
            WindowManagementOperation::Move => {
                if self.x.is_none()
                    || self.y.is_none()
                    || self.width.is_some()
                    || self.height.is_some()
                    || self.unit != Some(WindowManagementSpace::WindowsDesktopPixels)
                {
                    return Err("Move requires only x, y and windows_desktop_pixels unit".into());
                }
                if self.x.is_some_and(|x| !(-100000..=100000).contains(&x))
                    || self.y.is_some_and(|y| !(-100000..=100000).contains(&y))
                {
                    return Err("Requested position must be between -100000 and 100000".into());
                }
            }
            WindowManagementOperation::Resize => {
                if self.width.is_none()
                    || self.height.is_none()
                    || self.x.is_some()
                    || self.y.is_some()
                    || self.unit != Some(WindowManagementSpace::WindowsDesktopPixels)
                {
                    return Err(
                        "Resize requires only width, height and windows_desktop_pixels unit".into(),
                    );
                }
                if self.width.is_some_and(|w| !(1..=32767).contains(&w))
                    || self.height.is_some_and(|h| !(1..=32767).contains(&h))
                {
                    return Err("Requested dimensions must be between 1 and 32767".into());
                }
            }
            _ => {
                if self.x.is_some()
                    || self.y.is_some()
                    || self.width.is_some()
                    || self.height.is_some()
                    || self.unit.is_some()
                {
                    return Err(
                        "Window state and focus operations accept no geometry or unit".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Exact native source and one-use management state. Sampling grants no application-input authority.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowControlStateOutput {
    /// Process that owns the sampled original window lifetime.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact sampled native window, not another window of the same process.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// One-use control admission; it grants neither pixel nor typing authority.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// Measured geometry, native state and independently queried desktop membership.
    pub state: WindowControlSample,
}
impl ToolOutput for WindowControlStateOutput {
    fn validate(&self) -> Result<(), String> {
        validate_identity(self.pid, self.window_id)?;
        validate_state_id(&self.state_id)?;
        validate_source_sample(&self.state, self.window_id)
    }
}

/// Native window APIs only; never injected input or assisted keyboard activation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowManagementRoute {
    Native,
}
/// Confirms the requested native postcondition, not application pixels or permanent foreground.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowManagementEffect {
    Confirmed,
}

/// Exact-source successful mutation with separate request, completion and measured state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManagementOutput {
    /// Process that owns the admitted target.
    #[schemars(schema_with = "pid_schema")]
    pub pid: u32,
    /// Exact admitted native window.
    #[schemars(schema_with = "window_schema")]
    pub window_id: u64,
    /// Consumed admission identity echoed for exact receipt matching.
    #[schemars(schema_with = "state_schema")]
    pub state_id: String,
    /// Single requested native operation.
    pub operation: WindowManagementOperation,
    /// Complete requested frame in the before sample's bounds source; null for non-frame operations.
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_bounds_schema")]
    pub requested_bounds: Option<DesktopWindowBounds>,
    /// A native mutation was submitted or its submission outcome is uncertain.
    pub mutation_attempted: bool,
    /// The native backend accepted the single request, not proof that it completed.
    pub mutation_accepted: bool,
    /// Bounded readback independently confirmed the requested postcondition.
    pub postcondition_confirmed: bool,
    /// Pre-request native measurements; no retained pixel authority is implied.
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_sample_schema")]
    pub before: Option<WindowControlSample>,
    /// Post-request measured native effects, not fresh paint or permanent foreground.
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_sample_schema")]
    pub observed: Option<WindowControlSample>,
    /// Native window APIs, without input fallback.
    pub route: WindowManagementRoute,
    /// Confirms native state only; read the target again for pixels.
    pub effect: WindowManagementEffect,
}
impl ToolOutput for ManagementOutput {
    fn validate(&self) -> Result<(), String> {
        validate_identity(self.pid, self.window_id)?;
        validate_state_id(&self.state_id)?;
        if !self.mutation_attempted || !self.mutation_accepted || !self.postcondition_confirmed {
            return Err("Confirmed management requires every mutation prefix".into());
        }
        let before = self
            .before
            .as_ref()
            .ok_or("Confirmed management requires before metadata")?;
        let observed = self
            .observed
            .as_ref()
            .ok_or("Confirmed management requires observed metadata")?;
        validate_source_sample(before, self.window_id)?;
        validate_source_sample(observed, self.window_id)?;
        match self.operation {
            WindowManagementOperation::Move | WindowManagementOperation::Resize => {
                let requested = self
                    .requested_bounds
                    .ok_or("Frame operation requires requested bounds")?;
                requested.validate()?;
                if before.minimized
                    || before.maximized
                    || observed.minimized
                    || observed.maximized
                    || before.bounds_source != observed.bounds_source
                    || observed.bounds != requested
                {
                    return Err(
                        "Frame postcondition must match requested normal-window bounds and source"
                            .into(),
                    );
                }
                if self.operation == WindowManagementOperation::Move {
                    if requested.width != before.bounds.width
                        || requested.height != before.bounds.height
                        || !(-100000..=100000).contains(&requested.x)
                        || !(-100000..=100000).contains(&requested.y)
                    {
                        return Err(
                            "Move must preserve dimensions and respect requested position limits"
                                .into(),
                        );
                    }
                } else if requested.x != before.bounds.x || requested.y != before.bounds.y {
                    return Err("Resize must preserve position".into());
                }
            }
            operation => {
                if self.requested_bounds.is_some() {
                    return Err("Non-frame operation must not claim requested bounds".into());
                }
                let confirmed = match operation {
                    WindowManagementOperation::Minimize => observed.minimized,
                    WindowManagementOperation::Maximize => {
                        observed.maximized && !observed.minimized
                    }
                    WindowManagementOperation::Focus => {
                        observed.foreground_at_target && !observed.minimized
                    }
                    _ => unreachable!(),
                };
                if !confirmed {
                    return Err(
                        "Observed window state does not confirm the requested operation".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn contracts() -> Vec<ToolContract> {
    vec![
        ToolContract {
            name: WindowControlStateInput::TOOL_NAME.into(),
            description: "Use get_window_control_state to read exact Windows window geometry and state without pixels, a UI walk, activation or input. The one-use management state grants neither pointer nor typing authority.".into(),
            platforms: vec![Platform::Windows], aliases: vec![], capabilities: vec!["window.state".into()],
            annotations: ToolAnnotations { read_only: true, destructive: false, idempotent: false, open_world: true },
            schema_mode: SchemaMode::CanonicalRuntime, cursor_semantics: None,
            input_schema: WindowControlStateInput::input_schema(), success_output_schema: Some(WindowControlStateOutput::output_schema()),
            error_output_schema: None, output_validator: crate::validate_typed_output::<WindowControlStateOutput>,
        },
        ToolContract {
            name: WindowManagementInput::TOOL_NAME.into(),
            description: "Use manage_window to move, resize, minimize, maximize or focus one admitted Windows window through native APIs. Focus never restores a minimized target or injects keys. No implicit desktop switch or previous foreground restoration occurs. Retain interrupted prefixes, never replay an uncertain request, and read state again before deciding.".into(),
            platforms: vec![Platform::Windows], aliases: vec![], capabilities: vec!["window.manage".into()],
            annotations: ToolAnnotations { read_only: false, destructive: true, idempotent: false, open_world: true },
            schema_mode: SchemaMode::CanonicalRuntime, cursor_semantics: None,
            input_schema: WindowManagementInput::input_schema(), success_output_schema: Some(ManagementOutput::output_schema()),
            error_output_schema: None, output_validator: crate::validate_typed_output::<ManagementOutput>,
        },
    ]
}
