//! Metadata-only window state and explicit restore/activation. No injected-input fallback.

use super::{exact_window_ownership_result, ToolState};
use crate::window_restore_state::{Admission, Lifetime, Store, Target};
use async_trait::async_trait;
use cua_driver_contract::{
    ToolInput, ToolOutput, WindowRestoreEffect, WindowRestoreInput, WindowRestoreOutput,
    WindowRestoreRoute, WindowStatusInput, WindowStatusOutput,
};
use cua_driver_core::pointer::Failure;
use cua_driver_core::protocol::ToolResult;
use cua_driver_core::tool::{Tool, ToolDef};
use cua_driver_core::tool_args::parse_typed_input;
use cua_driver_core::window_restore::{self, Backend};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetPropW, IsChild, IsIconic, IsWindowVisible,
    RemovePropW, SetPropW, ShowWindowAsync, GUITHREADINFO, SW_RESTORE,
};

pub(super) fn failure(code: &'static str, detail: impl Into<String>) -> Failure {
    Failure {
        code,
        detail: detail.into(),
    }
}

fn refusal(code: &str, detail: impl Into<String>) -> ToolResult {
    let detail = detail.into();
    ToolResult::error(detail.clone()).with_structured(json!({
        "status":"refused", "code":code, "detail":detail, "effect":"refused",
        "route":"native", "restore_attempted":false, "restore_requested":false,
        "restored":false, "activation_attempted":false, "activation_succeeded":false,
        "visible":null, "foreground_at_target":null, "keyboard_focus_at_target":null,
        "foreground_changed":null,
    }))
}

/// Bind a known pre-mutation refusal to the exact attempted restore source.
fn restore_refusal(mut result: ToolResult, pid: u32, window_id: u64, state_id: &str) -> ToolResult {
    let payload = result
        .structured_content
        .take()
        .unwrap_or_else(|| json!({}));
    result.with_structured(restore_refusal_payload(payload, pid, window_id, state_id))
}

fn restore_refusal_payload(mut payload: Value, pid: u32, window_id: u64, state_id: &str) -> Value {
    payload["pid"] = json!(pid);
    payload["window_id"] = json!(window_id);
    payload["state_id"] = json!(state_id);
    payload["status"] = json!("refused");
    payload["effect"] = json!("refused");
    payload["route"] = json!("native");
    for field in [
        "restore_attempted",
        "restore_requested",
        "restored",
        "activation_attempted",
        "activation_succeeded",
    ] {
        payload[field] = json!(false);
    }
    for field in [
        "visible",
        "foreground_at_target",
        "keyboard_focus_at_target",
        "foreground_changed",
    ] {
        payload[field] = Value::Null;
    }
    payload
}

fn owner(pid: u32, window_id: u64) -> bool {
    crate::win32::window_owner_pid(window_id) == Some(pid)
}

pub(super) fn cancelled(flag: &AtomicBool) -> Result<(), Failure> {
    if flag.load(Ordering::Acquire) {
        Err(failure(
            "cancelled",
            "Cancelled; no fallback or automatic replay",
        ))
    } else {
        Ok(())
    }
}

/// Each property belongs to one exact original window lifetime, not its reusable HWND.
pub(super) struct Marker {
    pub(super) pid: u32,
    pub(super) window_id: u64,
    property: Vec<u16>,
}

impl Marker {
    pub(super) fn create(pid: u32, window_id: u64) -> Result<(String, Self), Failure> {
        if !owner(pid, window_id) {
            return Err(failure(
                "window_state_expired",
                "Exact window ownership changed",
            ));
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|error| failure("window_state_unavailable", error.to_string()))?;
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let state_id = format!("ws-{}", hex(&random[..16]));
        let property: Vec<_> = format!("CuaDriver.WindowState.{}", hex(&random[16..]))
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let marker = Self {
            pid,
            window_id,
            property,
        };
        unsafe { SetPropW(marker.hwnd(), marker.key(), HANDLE(1usize as *mut _)) }.map_err(
            |error| {
                failure(
                    "window_state_unavailable",
                    format!("Cannot retain exact window lifetime: {error}"),
                )
            },
        )?;
        if !marker.is_current() {
            return Err(failure(
                "window_state_expired",
                "Window changed during state lifetime admission",
            ));
        }
        Ok((state_id, marker))
    }

    pub(super) fn hwnd(&self) -> HWND {
        HWND(self.window_id as *mut _)
    }
    fn key(&self) -> PCWSTR {
        PCWSTR(self.property.as_ptr())
    }
}

impl Lifetime for Marker {
    fn is_current(&self) -> bool {
        owner(self.pid, self.window_id)
            && unsafe { GetPropW(self.hwnd(), self.key()) }.0 as usize == 1
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        // Never remove a replacement window's property or an independently replaced value.
        if self.is_current() {
            let _ = unsafe { RemovePropW(self.hwnd(), self.key()) };
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Sample {
    pub(super) minimized: bool,
    pub(super) visible: bool,
    pub(super) foreground_window: u64,
    pub(super) foreground_at_target: bool,
    pub(super) keyboard_focus_at_target: Option<bool>,
}

pub(super) fn sample(marker: &Marker) -> Result<Sample, Failure> {
    if !marker.is_current() {
        return Err(failure(
            "window_state_expired",
            "Original window lifetime no longer exists",
        ));
    }
    let hwnd = marker.hwnd();
    let mut gui = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let focus = unsafe { GetGUIThreadInfo(0, &mut gui) }
        .ok()
        .map(|()| gui.hwndFocus == hwnd || unsafe { IsChild(hwnd, gui.hwndFocus) }.as_bool());
    let foreground = unsafe { GetForegroundWindow() };
    let state = Sample {
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        foreground_window: foreground.0 as u64,
        foreground_at_target: foreground == hwnd,
        keyboard_focus_at_target: focus,
    };
    if !marker.is_current() {
        return Err(failure(
            "window_state_expired",
            "Window changed during state sampling",
        ));
    }
    Ok(state)
}

/// Bounded private state admissions; eviction and runtime drop release owned lifetime markers.
pub(super) struct Manager {
    states: Mutex<Store<Marker>>,
}

impl Manager {
    pub(super) fn new() -> Self {
        Self {
            states: Mutex::new(Store::default()),
        }
    }

    fn observe(&self, target: Target, flag: &AtomicBool) -> Result<WindowStatusOutput, Failure> {
        cancelled(flag)?;
        let (state_id, marker) = Marker::create(target.pid, target.window_id)?;
        cancelled(flag)?;
        let current = sample(&marker)?;
        cancelled(flag)?;
        let output = WindowStatusOutput {
            pid: target.pid,
            window_id: target.window_id,
            state_id: state_id.clone(),
            minimized: current.minimized,
            visible: current.visible,
            foreground_at_target: current.foreground_at_target,
            keyboard_focus_at_target: current.keyboard_focus_at_target,
        };
        self.states
            .lock()
            .map_err(|_| {
                failure(
                    "window_state_unavailable",
                    "Window state cache is unavailable",
                )
            })?
            .issue(
                state_id,
                Admission {
                    target,
                    minimized: current.minimized,
                    allow_management: false,
                    lifetime: marker,
                },
            );
        Ok(output)
    }

    /// Fresh metadata shares restore's original-window lease and bounded admission cache.
    pub(super) fn observe_control(
        &self,
        target: Target,
        flag: &AtomicBool,
    ) -> Result<cua_driver_contract::WindowControlStateOutput, Failure> {
        cancelled(flag)?;
        let (state_id, marker) = Marker::create(target.pid, target.window_id)?;
        cancelled(flag)?;
        let current = super::window_management::sample_control(&marker)?;
        cancelled(flag)?;
        let output = cua_driver_contract::WindowControlStateOutput {
            pid: target.pid,
            window_id: target.window_id,
            state_id: state_id.clone(),
            state: current,
        };
        output
            .validate()
            .map_err(|detail| failure("window_control_state_unavailable", detail))?;
        self.states
            .lock()
            .map_err(|_| {
                failure(
                    "window_state_unavailable",
                    "Window state cache is unavailable",
                )
            })?
            .issue(
                state_id,
                Admission {
                    target,
                    minimized: output.state.minimized,
                    allow_management: true,
                    lifetime: marker,
                },
            );
        Ok(output)
    }

    /// Consume a control-purpose reference once; ordinary restore can consume the same lease.
    pub(super) fn admit_control(
        &self,
        target: &Target,
        state_id: &str,
    ) -> Result<Admission<Marker>, Failure> {
        self.states.lock().map_err(|_| failure("window_state_unavailable", "Window state cache is unavailable"))?
            .consume_control(state_id, target)
            .map_err(|error| failure(error.code(), "Read fresh control state for this exact window; references are one-use and cannot borrow restore-only authority"))
    }
    fn admit(&self, target: &Target, state_id: &str) -> Result<Admission<Marker>, Failure> {
        self.states
            .lock()
            .map_err(|_| {
                failure(
                    "window_state_unavailable",
                    "Window state cache is unavailable",
                )
            })?
            .consume(state_id, target)
            .map_err(|error| {
                failure(
                    error.code(),
                    "Window state is expired, consumed or belongs to another exact target",
                )
            })
    }
}

pub(super) fn target(args: &Value, pid: u32, window_id: u64) -> Result<Target, ToolResult> {
    // Public labels cannot substitute for another transport's private state owner.
    let session = args
        .get("_session_id")
        .and_then(Value::as_str)
        .filter(|session| !session.trim().is_empty())
        .ok_or_else(|| {
            refusal(
                "window_state_session_unavailable",
                "Window state needs the authenticated transport session",
            )
        })?;
    Ok(Target {
        pid,
        window_id,
        session: session.to_owned(),
    })
}

pub(super) struct GetWindowStatusTool {
    pub state: Arc<ToolState>,
}

#[async_trait]
impl Tool for GetWindowStatusTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("get_window_status")
                    .expect("window status contract"),
            )
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let input: WindowStatusInput = match parse_typed_input("get_window_status", args.clone()) {
            Ok(input) => input,
            Err(error) => return error,
        };
        if let Err(detail) = input.validate() {
            return refusal("invalid_arguments", detail);
        }
        let target = match target(&args, input.pid, input.window_id) {
            Ok(target) => target,
            Err(error) => return error,
        };
        let flag = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(flag.clone());
        let state = self.state.clone();
        match tokio::task::spawn_blocking(move || state.window_status.observe(target, &flag)).await {
            Ok(Ok(output)) => ToolResult::text("Fresh exact-window metadata; no screenshot, UI tree or activation. The state reference grants only one explicit restore operation.")
                .with_structured(serde_json::to_value(output).expect("window status serializes")),
            Ok(Err(error)) => refusal(error.code, error.detail),
            Err(error) => refusal("window_state_unavailable", format!("Window state worker failed: {error}")),
        }
    }
}

struct NativeBackend {
    admitted: Admission<Marker>,
    flag: Arc<AtomicBool>,
    started: Instant,
    requested: bool,
}

impl NativeBackend {
    fn before_request(&self) -> Result<(), Failure> {
        cancelled(&self.flag)?;
        if !self.admitted.lifetime.is_current() {
            return Err(failure(
                "window_state_expired",
                "Original window lifetime no longer exists",
            ));
        }
        if !self.requested {
            if !self.admitted.minimized {
                return Err(failure(
                    "window_not_minimized",
                    "This operation only restores an observed minimized window",
                ));
            }
            if !unsafe { IsIconic(self.admitted.lifetime.hwnd()) }.as_bool() {
                return Err(failure(
                    "window_state_changed",
                    "Window is no longer in the observed minimized state; read it again",
                ));
            }
        }
        // Native ownership/state queries may be slow; cancellation must be checked last.
        cancelled(&self.flag)
    }
}

impl Backend for NativeBackend {
    fn check_target(&mut self) -> Result<(), Failure> {
        self.before_request()
    }

    fn request_restore(&mut self) -> Result<(), Failure> {
        // Repeat the cancellation/lifetime check immediately before the asynchronous OS request.
        self.before_request()?;
        let accepted =
            unsafe { ShowWindowAsync(self.admitted.lifetime.hwnd(), SW_RESTORE) }.as_bool();
        self.requested = true;
        if accepted {
            Ok(())
        } else {
            Err(failure("restore_request_unconfirmed", "Windows did not confirm the asynchronous restore request; its effect may be incomplete"))
        }
    }

    fn is_restored(&mut self) -> Result<bool, Failure> {
        if !self.admitted.lifetime.is_current() {
            return Err(failure(
                "window_state_expired",
                "Window lifetime changed during restoration",
            ));
        }
        let restored = !unsafe { IsIconic(self.admitted.lifetime.hwnd()) }.as_bool();
        if !self.admitted.lifetime.is_current() {
            return Err(failure(
                "window_state_expired",
                "Window changed during restoration readback",
            ));
        }
        Ok(restored)
    }

    fn activate_once(&mut self) -> Result<(), Failure> {
        self.before_request()?;
        if !unsafe { crate::input::force_foreground_attached(self.admitted.lifetime.hwnd()) } {
            return Err(failure("foreground_unavailable", "Windows refused foreground activation after restore; no keyboard or pointer fallback was attempted"));
        }
        Ok(())
    }

    fn is_foreground(&mut self) -> Result<bool, Failure> {
        Ok(sample(&self.admitted.lifetime)?.foreground_at_target)
    }
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn wait_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

pub(super) struct RestoreWindowTool {
    pub state: Arc<ToolState>,
}

#[async_trait]
impl Tool for RestoreWindowTool {
    fn def(&self) -> &ToolDef {
        static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("restore_window")
                    .expect("window restore contract"),
            )
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let input: WindowRestoreInput = match parse_typed_input("restore_window", args.clone()) {
            Ok(input) => input,
            Err(error) => return error,
        };
        if let Err(detail) = input.validate() {
            return restore_refusal(
                refusal("invalid_arguments", detail),
                input.pid,
                input.window_id,
                &input.state_id,
            );
        }
        let target = match target(&args, input.pid, input.window_id) {
            Ok(target) => target,
            Err(error) => {
                return restore_refusal(error, input.pid, input.window_id, &input.state_id)
            }
        };
        let admitted = match self.state.window_status.admit(&target, &input.state_id) {
            Ok(admitted) => admitted,
            Err(error) => {
                return restore_refusal(
                    refusal(error.code, error.detail),
                    input.pid,
                    input.window_id,
                    &input.state_id,
                )
            }
        };
        let flag = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::input::path::CancelOnDrop(flag.clone());
        let admission = cua_driver_core::tool::retain_desktop_action_admission();
        let pid = input.pid;
        let window_id = input.window_id;
        let state_id = input.state_id;
        let pending_state_id = state_id.clone();
        match tokio::task::spawn_blocking(move || {
            let _admission = admission;
            if let Err(error) = exact_window_ownership_result(pid, window_id, crate::win32::window_owner_pid(window_id)) {
                return restore_refusal(error, pid, window_id, &state_id);
            }
            let before = match sample(&admitted.lifetime) {
                Ok(before) => before,
                Err(error) => return restore_refusal(refusal(error.code, error.detail), pid, window_id, &state_id),
            };
            let mut backend = NativeBackend { admitted, flag, started: Instant::now(), requested: false };
            let report = window_restore::execute(&mut backend);
            // Readonly final measurement does not renew admission and may retain a restoration prefix after cancellation.
            let after = sample(&backend.admitted.lifetime).ok();
            let restored = report.restored || (backend.requested && after.is_some_and(|after| !after.minimized));
            let output = WindowRestoreOutput {
                pid, window_id, state_id,
                restore_attempted: report.restore_attempted,
                restore_requested: report.restore_requested,
                restored,
                activation_attempted: report.activation_attempted,
                activation_succeeded: report.activation_succeeded,
                visible: after.map(|after| after.visible),
                foreground_at_target: after.map(|after| after.foreground_at_target),
                keyboard_focus_at_target: after.and_then(|after| after.keyboard_focus_at_target),
                foreground_changed: after.map(|after| before.foreground_window != after.foreground_window),
                route: WindowRestoreRoute::Native, effect: WindowRestoreEffect::Confirmed,
            };
            let mut payload = serde_json::to_value(&output).expect("window restore serializes");
            let error = report.failure.or_else(|| output.validate().err().map(|detail| failure("restore_confirmation_unavailable", detail)));
            if let Some(error) = error {
                let unknown = report.restore_attempted;
                payload["status"] = json!(if unknown { "unknown" } else { "refused" });
                payload["effect"] = json!(if unknown { "unknown" } else { "refused" });
                payload["code"] = json!(error.code);
                payload["detail"] = json!(error.detail);
                return ToolResult::error("Restore did not complete all confirmations. Read fresh window state; never replay a possibly completed prefix.").with_structured(payload);
            }
            ToolResult::text("Exact minimized window restored and explicitly activated. Native visibility and foreground were measured; keyboard focus may be unknown. This is not proof of fresh paint. No injected-input fallback or automatic foreground/pointer restoration occurred.")
                .with_structured(payload)
        }).await {
            Ok(result) => result,
            Err(error) => ToolResult::error(format!("Restore worker outcome unknown: {error}. Do not replay."))
                .with_structured(json!({ "status":"unknown", "effect":"unknown", "route":"native", "code":"restore_worker_unavailable",
                    "pid":pid, "window_id":window_id, "state_id":pending_state_id,
                    "detail":format!("Restore worker outcome unavailable: {error}"),
                    "restore_attempted":null, "restore_requested":null,
                    "restored":null, "activation_attempted":null, "activation_succeeded":null,
                    "visible":null, "foreground_at_target":null, "keyboard_focus_at_target":null,
                    "foreground_changed":null })),
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    #[test]
    fn pre_mutation_refusal_retains_the_exact_source_and_unknown_measurements() {
        let payload = restore_refusal_payload(
            json!({
                "code":"window_state_expired", "detail":"Original lifetime expired"
            }),
            42,
            91,
            "native-state",
        );
        assert_eq!(payload["pid"], 42);
        assert_eq!(payload["window_id"], 91);
        assert_eq!(payload["state_id"], "native-state");
        assert_eq!(payload["code"], "window_state_expired");
        assert_eq!(payload["detail"], "Original lifetime expired");
        assert_eq!(payload["route"], "native");
        assert_eq!(payload["status"], "refused");
        assert_eq!(payload["effect"], "refused");
        for field in [
            "restore_attempted",
            "restore_requested",
            "restored",
            "activation_attempted",
            "activation_succeeded",
        ] {
            assert_eq!(payload[field], false, "{field}");
        }
        for field in [
            "visible",
            "foreground_at_target",
            "keyboard_focus_at_target",
            "foreground_changed",
        ] {
            assert_eq!(payload[field], Value::Null, "{field}");
        }
    }

    #[test]
    fn ownership_refusal_keeps_diagnostics_and_adds_the_complete_no_mutation_receipt() {
        let payload = restore_refusal_payload(
            json!({
                "code":"window_target_mismatch", "owner_pid":99,
                "detail":"Exact owner changed", "pid":42, "window_id":91, "effect":"refused"
            }),
            42,
            91,
            "native-state",
        );
        assert_eq!(payload["owner_pid"], 99);
        assert_eq!(payload["code"], "window_target_mismatch");
        assert_eq!(payload["detail"], "Exact owner changed");
        assert_eq!(payload["state_id"], "native-state");
        assert_eq!(payload["restore_attempted"], false);
        assert_eq!(payload["activation_succeeded"], false);
        assert_eq!(payload["keyboard_focus_at_target"], Value::Null);
    }
}
