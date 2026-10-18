//! One-use, exact-window native management. No input, desktop switch or replay fallback.

use super::window_restore::{cancelled, failure, sample, target, Marker};
use super::{outer_frame_for_visible_request, ToolState};
use crate::window_restore_state::{Admission, Lifetime};
use async_trait::async_trait;
use cua_driver_contract::{
    DesktopWindowBounds, ManagementOutput, WindowBoundsSource, WindowControlSample,
    WindowControlStateInput, WindowManagementEffect, WindowManagementInput,
    WindowManagementOperation, WindowManagementRoute, WindowManagementSpace,
};
use cua_driver_contract::{ToolInput, ToolOutput};
use cua_driver_core::pointer::Failure;
use cua_driver_core::protocol::ToolResult;
use cua_driver_core::tool::{Tool, ToolDef};
use cua_driver_core::tool_args::parse_typed_input;
use cua_driver_core::window_management::{self, Backend};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsZoomed, SetWindowPos, ShowWindowAsync, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_MAXIMIZE, SW_SHOWMINNOACTIVE,
};

fn checked_bounds(rect: RECT) -> Result<DesktopWindowBounds, Failure> {
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if width <= 0 || height <= 0 || width > i64::from(i32::MAX) || height > i64::from(i32::MAX) {
        return Err(failure(
            "window_bounds_unavailable",
            "Windows returned an invalid or oversized window frame",
        ));
    }
    Ok(DesktopWindowBounds {
        x: rect.left,
        y: rect.top,
        width: width as u32,
        height: height as u32,
    })
}

/// Read visible-frame and outer geometry separately; only the driver owns DPI conversion.
fn frames(
    marker: &Marker,
) -> Result<(DesktopWindowBounds, DesktopWindowBounds, WindowBoundsSource), Failure> {
    if !marker.is_current() {
        return Err(failure(
            "window_state_expired",
            "Original window lifetime no longer exists",
        ));
    }
    let mut outer = RECT::default();
    unsafe { GetWindowRect(marker.hwnd(), &mut outer) }
        .map_err(|error| failure("window_bounds_unavailable", error.to_string()))?;
    let outer = checked_bounds(outer)?;
    let mut visible = RECT::default();
    let visible = unsafe {
        DwmGetWindowAttribute(
            marker.hwnd(),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut visible as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .ok()
    .and_then(|()| checked_bounds(visible).ok());
    if !marker.is_current() {
        return Err(failure(
            "window_state_expired",
            "Window changed during geometry sampling",
        ));
    }
    Ok(match visible {
        Some(visible) => (outer, visible, WindowBoundsSource::DwmVisibleFrame),
        None => (outer, outer, WindowBoundsSource::WindowRect),
    })
}

/// Metadata only. Desktop-query errors and unknown keyboard focus remain explicit.
pub(super) fn sample_control(marker: &Marker) -> Result<WindowControlSample, Failure> {
    let current = sample(marker)?;
    let (_, bounds, bounds_source) = frames(marker)?;
    let maximized = unsafe { IsZoomed(marker.hwnd()) }.as_bool();
    let virtual_desktop = crate::shell_desktop::Query::new().read(marker.pid, marker.window_id);
    if !marker.is_current() {
        return Err(failure(
            "window_state_expired",
            "Window changed during control-state sampling",
        ));
    }
    Ok(WindowControlSample {
        bounds,
        bounds_source,
        coordinate_space: WindowManagementSpace::WindowsDesktopPixels,
        minimized: current.minimized,
        maximized,
        visible: current.visible,
        foreground_at_target: current.foreground_at_target,
        keyboard_focus_at_target: current.keyboard_focus_at_target,
        foreground_window: current.foreground_window,
        virtual_desktop,
    })
}

/// Preserve diagnostics while binding a known no-mutation refusal to its attempted source.
pub(super) fn bind_refusal(mut result: ToolResult, args: &Value) -> ToolResult {
    let payload = result
        .structured_content
        .take()
        .unwrap_or_else(|| json!({}));
    result.with_structured(refusal_payload(payload, args))
}

fn refusal_payload(mut payload: Value, args: &Value) -> Value {
    for field in ["pid", "window_id", "state_id", "operation"] {
        payload[field] = args.get(field).cloned().unwrap_or(Value::Null);
    }
    payload["route"] = json!("native");
    payload["status"] = json!("refused");
    payload["effect"] = json!("refused");
    for field in [
        "mutation_attempted",
        "mutation_accepted",
        "postcondition_confirmed",
    ] {
        payload[field] = json!(false);
    }
    for field in ["requested_bounds", "before", "observed"] {
        payload[field] = Value::Null;
    }
    payload
}

fn refusal(input: &WindowManagementInput, error: Failure) -> ToolResult {
    bind_refusal(
        ToolResult::error(error.detail.clone()).with_structured(json!({
            "code":error.code, "detail":error.detail,
        })),
        &serde_json::to_value(input).expect("management input serializes"),
    )
}

pub(super) struct GetWindowControlStateTool {
    pub state: Arc<ToolState>,
}

#[async_trait]
impl Tool for GetWindowControlStateTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("get_window_control_state")
                    .expect("window control state contract"),
            )
        })
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let input: WindowControlStateInput =
            match parse_typed_input("get_window_control_state", args.clone()) {
                Ok(input) => input,
                Err(error) => return error,
            };
        if let Err(detail) = input.validate() {
            return ToolResult::error(detail);
        }
        let target = match target(&args, input.pid, input.window_id) {
            Ok(target) => target,
            Err(error) => return error,
        };
        let flag = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(flag.clone());
        let state = self.state.clone();
        match tokio::task::spawn_blocking(move || state.window_status.observe_control(target, &flag)).await {
            Ok(Ok(output)) => ToolResult::text("Fresh exact-window control metadata; no pixels, UI, activation or desktop switch. This one-use reference admits native management or existing minimized-window restore.")
                .with_structured(serde_json::to_value(output).expect("window control state serializes")),
            Ok(Err(error)) => ToolResult::error(error.detail.clone()).with_structured(json!({
                "pid":input.pid,"window_id":input.window_id,"code":error.code,"detail":error.detail,
                "status":"refused","effect":"refused",
            })),
            Err(error) => ToolResult::error(format!("Control-state worker unavailable: {error}")),
        }
    }
}

struct NativeBackend {
    admitted: Admission<Marker>,
    flag: Arc<AtomicBool>,
    started: Instant,
    initial: Option<WindowControlSample>,
    mutation_attempted: bool,
    mutation_accepted: bool,
}

impl NativeBackend {
    fn alive(&self) -> Result<(), Failure> {
        cancelled(&self.flag)?;
        if !self.admitted.lifetime.is_current() {
            return Err(failure(
                "window_state_expired",
                "Original window lifetime no longer exists",
            ));
        }
        cancelled(&self.flag)
    }

    fn before_request(
        &self,
        operation: WindowManagementOperation,
    ) -> Result<WindowControlSample, Failure> {
        self.alive()?;
        let current = sample_control(&self.admitted.lifetime)?;
        match operation {
            WindowManagementOperation::Move | WindowManagementOperation::Resize => {
                if current.minimized || current.maximized {
                    return Err(failure("window_not_normal", "Frame changes require a normal window; use restore_window for a minimized target"));
                }
                if let Some(before) = &self.initial {
                    if current.bounds != before.bounds
                        || current.bounds_source != before.bounds_source
                    {
                        return Err(failure("window_state_changed", "Window geometry or its measurement source changed; read fresh control state"));
                    }
                }
            }
            WindowManagementOperation::Maximize | WindowManagementOperation::Focus => {
                if current.minimized {
                    return Err(failure("window_minimized", "Use restore_window with fresh state; management never implicitly restores a minimized target"));
                }
                if current.virtual_desktop.on_current_desktop != Some(true) {
                    return Err(failure("window_desktop_unavailable", "Activation requires confirmed current-desktop membership; no desktop switch or input fallback is allowed"));
                }
            }
            WindowManagementOperation::Minimize => {}
        }
        self.alive()?;
        Ok(current)
    }
}

fn tuple(bounds: DesktopWindowBounds) -> (i32, i32, i32, i32) {
    (
        bounds.x,
        bounds.y,
        bounds.width as i32,
        bounds.height as i32,
    )
}

impl Backend for NativeBackend {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn check_target(&mut self) -> Result<(), Failure> {
        self.alive()
    }
    fn sample(&mut self) -> Result<WindowControlSample, Failure> {
        self.alive()?;
        let current = sample_control(&self.admitted.lifetime)?;
        self.alive()?;
        if self.initial.is_none() {
            self.initial = Some(current.clone());
        }
        Ok(current)
    }
    fn request(
        &mut self,
        operation: WindowManagementOperation,
        requested: Option<DesktopWindowBounds>,
    ) -> Result<(), Failure> {
        let current = self.before_request(operation)?;
        let hwnd = self.admitted.lifetime.hwnd();
        match operation {
            WindowManagementOperation::Move | WindowManagementOperation::Resize => {
                let requested = requested.ok_or_else(|| {
                    failure(
                        "invalid_arguments",
                        "Frame request is missing requested bounds",
                    )
                })?;
                let (outer, visible, source) = frames(&self.admitted.lifetime)?;
                if visible != current.bounds || source != current.bounds_source {
                    return Err(failure(
                        "window_state_changed",
                        "Frame changed before native submission; read fresh state",
                    ));
                }
                let adjusted =
                    outer_frame_for_visible_request(tuple(requested), tuple(outer), tuple(visible))
                        .map_err(|detail| failure("window_bounds_unavailable", detail))?;
                self.alive()?;
                self.mutation_attempted = true;
                // Avoid blocking on another input queue; acceptance is confirmed only by bounded readback.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        HWND::default(),
                        adjusted.0,
                        adjusted.1,
                        adjusted.2,
                        adjusted.3,
                        SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS,
                    )
                }
                .map_err(|error| {
                    failure(
                        "window_management_unconfirmed",
                        format!("SetWindowPos outcome incomplete: {error}"),
                    )
                })?;
            }
            WindowManagementOperation::Minimize | WindowManagementOperation::Maximize => {
                // Async work may outlive cancellation; submission is not completion evidence.
                self.alive()?;
                self.mutation_attempted = true;
                let show = if operation == WindowManagementOperation::Minimize {
                    SW_SHOWMINNOACTIVE
                } else {
                    SW_MAXIMIZE
                };
                if !unsafe { ShowWindowAsync(hwnd, show) }.as_bool() {
                    return Err(failure("window_management_unconfirmed", "Windows did not confirm the asynchronous window-state request; never replay it"));
                }
            }
            WindowManagementOperation::Focus => {
                self.alive()?;
                self.mutation_attempted = true;
                if !unsafe { crate::input::force_foreground_attached(hwnd) } {
                    return Err(failure("foreground_unavailable", "Windows refused foreground activation; no restore, keys, pointer or replay fallback was attempted"));
                }
            }
        }
        self.mutation_accepted = true;
        Ok(())
    }
    fn wait_ms(&mut self, ms: u64) -> Result<(), Failure> {
        self.alive()?;
        std::thread::sleep(Duration::from_millis(ms));
        self.alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_mutation_refusal_retains_exact_source_and_diagnostic_without_invented_measurements() {
        let input = json!({"pid":42,"window_id":91,"state_id":"control-state","operation":"move"});
        let payload = refusal_payload(
            json!({"code":"window_state_expired","detail":"Lifetime changed","owner_pid":7}),
            &input,
        );
        for field in ["pid", "window_id", "state_id", "operation"] {
            assert_eq!(payload[field], input[field], "{field}");
        }
        assert_eq!(payload["code"], "window_state_expired");
        assert_eq!(payload["detail"], "Lifetime changed");
        assert_eq!(payload["owner_pid"], 7);
        assert_eq!(payload["route"], "native");
        assert_eq!(payload["status"], "refused");
        assert_eq!(payload["effect"], "refused");
        for field in [
            "mutation_attempted",
            "mutation_accepted",
            "postcondition_confirmed",
        ] {
            assert_eq!(payload[field], false, "{field}");
        }
        for field in ["requested_bounds", "before", "observed"] {
            assert_eq!(payload[field], Value::Null, "{field}");
        }
    }

    #[test]
    fn pre_mutation_receipt_cannot_inherit_success_flags_or_foreign_source() {
        let payload = refusal_payload(
            json!({
                "pid":9,"state_id":"other","operation":"focus","status":"ok","effect":"confirmed",
                "mutation_attempted":true,"mutation_accepted":true,"postcondition_confirmed":true,
                "observed":{"foreground_at_target":true},
            }),
            &json!({"pid":42,"window_id":91,"state_id":"control-state","operation":"maximize"}),
        );
        assert_eq!(payload["pid"], 42);
        assert_eq!(payload["state_id"], "control-state");
        assert_eq!(payload["operation"], "maximize");
        assert_eq!(payload["mutation_attempted"], false);
        assert_eq!(payload["mutation_accepted"], false);
        assert_eq!(payload["postcondition_confirmed"], false);
        assert_eq!(payload["observed"], Value::Null);
        assert_eq!(payload["effect"], "refused");
    }
}
pub(super) struct ManageWindowTool {
    pub state: Arc<ToolState>,
}

#[async_trait]
impl Tool for ManageWindowTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("manage_window")
                    .expect("window management contract"),
            )
        })
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let input: WindowManagementInput = match parse_typed_input("manage_window", args.clone()) {
            Ok(input) => input,
            Err(error) => return bind_refusal(error, &args),
        };
        if let Err(detail) = input.validate() {
            return refusal(&input, failure("invalid_arguments", detail));
        }
        let target = match target(&args, input.pid, input.window_id) {
            Ok(target) => target,
            Err(error) => return bind_refusal(error, &args),
        };
        let admitted = match self
            .state
            .window_status
            .admit_control(&target, &input.state_id)
        {
            Ok(admitted) => admitted,
            Err(error) => return refusal(&input, error),
        };
        let flag = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(flag.clone());
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let pending = serde_json::to_value(&input).expect("management input serializes");
        match tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let mut backend = NativeBackend { admitted, flag, started: Instant::now(), initial: None,
                mutation_attempted: false, mutation_accepted: false };
            let report = window_management::execute(&input, &mut backend);
            // Read-only sampling after failure preserves completed native effects, not new authority.
            let observed = if report.failure.is_some() {
                sample_control(&backend.admitted.lifetime).ok().or(report.observed)
            } else {
                // Preserve the sampled confirmation, not a later snapshot after external changes.
                report.observed
            };
            let output = ManagementOutput {
                pid: input.pid, window_id: input.window_id, state_id: input.state_id,
                operation: input.operation, requested_bounds: report.requested_bounds,
                before: report.before, observed,
                mutation_attempted: backend.mutation_attempted,
                mutation_accepted: backend.mutation_accepted,
                postcondition_confirmed: report.postcondition_confirmed,
                route: WindowManagementRoute::Native, effect: WindowManagementEffect::Confirmed,
            };
            let mut payload = serde_json::to_value(&output).expect("management output serializes");
            // A final cancellation overrides completed flags. Native submission, not entry to request(), owns the prefix.
            let error = report.failure.or_else(|| output.validate().err()
                .map(|detail| failure("window_management_confirmation_unavailable", detail)));
            if let Some(error) = error {
                let effect = if backend.mutation_attempted { "unknown" } else { "refused" };
                payload["effect"] = json!(effect); payload["status"] = json!(effect);
                payload["code"] = json!(error.code); payload["detail"] = json!(error.detail);
                return ToolResult::error("Native window management did not complete all confirmations. Read fresh state; never replay a possibly completed prefix.")
                    .with_structured(payload);
            }
            ToolResult::text("Exact-window native management confirmed. Geometry, state, foreground and keyboard focus were measured separately. No input fallback, desktop switch or automatic foreground/pointer restoration occurred; this is not fresh-paint evidence.")
                .with_structured(payload)
        }).await {
            Ok(result) => result,
            Err(error) => {
                let mut payload = pending;
                // Unavailable worker outcome must never imply that no OS request was submitted.
                for field in ["x","y","width","height","unit","session"] { payload.as_object_mut().expect("input object").remove(field); }
                for field in ["mutation_attempted","mutation_accepted","postcondition_confirmed","requested_bounds","before","observed"] {
                    payload[field] = Value::Null;
                }
                payload["route"] = json!("native"); payload["status"] = json!("unknown"); payload["effect"] = json!("unknown");
                payload["code"] = json!("window_management_worker_unavailable"); payload["detail"] = json!(error.to_string());
                ToolResult::error(format!("Management worker outcome unknown: {error}. Do not replay.")).with_structured(payload)
            }
        }
    }
}
