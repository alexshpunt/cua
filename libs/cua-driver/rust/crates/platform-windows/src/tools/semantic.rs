//! UIA-only semantic operations; this module has no input-event dispatch path.
use super::impl_::ToolState;
use crate::semantic::{self, Control, Failure};
use async_trait::async_trait;
use cua_driver_contract::{SemanticActionInput, SemanticPattern, ToolInput};
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::parse_typed_input,
};
use serde_json::{json, Value};
use std::sync::Arc;
use windows::core::{Interface, BSTR};
use windows::Win32::UI::Accessibility::*;

pub(super) struct SemanticActionTool {
    pub state: Arc<ToolState>,
}
static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

fn failure(error: Failure) -> ToolResult {
    ToolResult::error(error.code).with_structured(json!({
        "status": if error.attempted { "unknown" } else { "refused" },
        "code":error.code, "attempted":error.attempted, "hresult":error.hresult,
        "route":"accessibility", "fallback":false,
    }))
}
fn query_error(error: windows::core::Error) -> Failure {
    if error.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32 {
        return Failure::refusal("semantic_element_stale");
    }
    Failure {
        code: "semantic_provider_unavailable",
        attempted: false,
        hresult: Some(error.code().0),
    }
}
fn pattern_error(error: windows::core::Error) -> Failure {
    if error.code().0 == UIA_E_NOTSUPPORTED as i32 {
        Failure::refusal("semantic_pattern_unsupported")
    } else {
        query_error(error)
    }
}

// Holds the actual pattern chosen during preparation. A failing call never selects another one.
enum Prepared {
    Invoke(IUIAutomationInvokePattern),
    Select(IUIAutomationSelectionItemPattern),
    Value(IUIAutomationValuePattern, String),
    Range(IUIAutomationRangeValuePattern, f64),
}
struct UiaControl<'a> {
    element: &'a IUIAutomationElement,
    hwnd: u64,
    pid: u32,
    prepared: Option<Prepared>,
}
impl UiaControl<'_> {
    fn check_window(&self) -> Result<(), Failure> {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow},
        };
        let hwnd = HWND(self.hwnd as *mut _);
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if !unsafe { IsWindow(hwnd) }.as_bool() || pid != self.pid {
            return Err(Failure::refusal("semantic_window_changed"));
        }
        Ok(())
    }
}
impl Control for UiaControl<'_> {
    fn enabled(&mut self) -> Result<bool, Failure> {
        self.check_window()?;
        if unsafe { self.element.CurrentProcessId() }.map_err(query_error)? != self.pid as i32 {
            return Err(Failure::refusal("semantic_element_process_changed"));
        }
        unsafe { self.element.CurrentIsEnabled() }
            .map(|value| value.as_bool())
            .map_err(query_error)
    }
    fn prepare(&mut self, pattern: SemanticPattern, value: Option<&str>) -> Result<(), Failure> {
        let id = match pattern {
            SemanticPattern::Invoke => UIA_InvokePatternId,
            SemanticPattern::SelectionItem => UIA_SelectionItemPatternId,
            SemanticPattern::Value => UIA_ValuePatternId,
            SemanticPattern::RangeValue => UIA_RangeValuePatternId,
        };
        // GetCurrentPattern may succeed with a null pointer for an unsupported
        // pattern. Keep that distinct from an unavailable provider.
        let mut pointer = std::ptr::null_mut();
        unsafe {
            (Interface::vtable(self.element).GetCurrentPattern)(
                Interface::as_raw(self.element),
                id,
                &mut pointer,
            )
            .ok()
            .map_err(pattern_error)?;
        }
        if pointer.is_null() {
            return Err(Failure::refusal("semantic_pattern_unsupported"));
        }
        let native = unsafe { windows::core::IUnknown::from_raw(pointer) };
        self.prepared = Some(match pattern {
            SemanticPattern::Invoke => Prepared::Invoke(native.cast().map_err(pattern_error)?),
            SemanticPattern::SelectionItem => {
                Prepared::Select(native.cast().map_err(pattern_error)?)
            }
            SemanticPattern::Value => {
                let pattern: IUIAutomationValuePattern = native.cast().map_err(pattern_error)?;
                if unsafe { pattern.CurrentIsReadOnly() }
                    .map_err(query_error)?
                    .as_bool()
                {
                    return Err(Failure::refusal("semantic_element_read_only"));
                }
                Prepared::Value(pattern, value.unwrap_or_default().to_owned())
            }
            SemanticPattern::RangeValue => {
                let pattern: IUIAutomationRangeValuePattern =
                    native.cast().map_err(pattern_error)?;
                if unsafe { pattern.CurrentIsReadOnly() }
                    .map_err(query_error)?
                    .as_bool()
                {
                    return Err(Failure::refusal("semantic_element_read_only"));
                }
                let value: f64 = value
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Failure::refusal("semantic_invalid_value"))?;
                let min = unsafe { pattern.CurrentMinimum() }.map_err(query_error)?;
                let max = unsafe { pattern.CurrentMaximum() }.map_err(query_error)?;
                if !min.is_finite() || !max.is_finite() || min > max {
                    return Err(Failure::refusal("semantic_invalid_range"));
                }
                if !value.is_finite() || value < min || value > max {
                    return Err(Failure::refusal("semantic_invalid_value"));
                }
                Prepared::Range(pattern, value)
            }
        });
        Ok(())
    }
    fn apply(&mut self) -> Result<(), Failure> {
        if !self.enabled()? {
            return Err(Failure::refusal("semantic_element_disabled"));
        }
        let Some(pattern) = self.prepared.take() else {
            return Err(Failure::refusal("semantic_pattern_unsupported"));
        };
        let _no_activate = crate::input::NoActivateGuard::arm(windows::Win32::Foundation::HWND(
            self.hwnd as *mut _,
        ));
        crate::uia::fg_bypass::run_with_uwp_bypass(self.hwnd as isize, || unsafe {
            match pattern {
                Prepared::Invoke(pattern) => pattern.Invoke(),
                Prepared::Select(pattern) => pattern.Select(),
                Prepared::Value(pattern, value) => pattern.SetValue(&BSTR::from(value.as_str())),
                Prepared::Range(pattern, value) => pattern.SetValue(value),
            }
        })
        .map_err(|error| Failure {
            code: "semantic_provider_failed",
            attempted: true,
            hresult: Some(error.code().0),
        })
    }
}

#[async_trait]
impl Tool for SemanticActionTool {
    fn def(&self) -> &ToolDef {
        DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("semantic_action").expect("semantic contract"),
            )
        })
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let input: SemanticActionInput = match parse_typed_input("semantic_action", args.clone()) {
            Ok(input) => input,
            Err(error) => return error,
        };
        if input.validate().is_err() {
            return failure(Failure::refusal("invalid_arguments"));
        }
        let resolved = match self.state.snapshots.resolve(input.pid as i32, &args) {
            Ok(target) => target,
            Err(error) => return error,
        };
        let (_, window_id, admitted) = resolved.into_parts(None);
        let Some(admitted) = admitted else {
            return failure(Failure::refusal("invalid_element_token"));
        };
        if window_id != Some(input.window_id) {
            return failure(Failure::refusal("conflicting_element_target"));
        }
        if !admitted.is_uia() {
            return failure(Failure::refusal("semantic_pattern_unsupported"));
        }
        let result = tokio::task::spawn_blocking(move || {
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            // RPC_E_CHANGED_MODE means COM is already initialized on this worker.
            if hr.is_err() && hr.0 != -2147417850 {
                return failure(query_error(windows::core::Error::from(hr)));
            }
            struct Apartment(bool);
            impl Drop for Apartment {
                fn drop(&mut self) {
                    if self.0 {
                        unsafe { CoUninitialize(); }
                    }
                }
            }
            let _apartment = Apartment(hr.is_ok());
            let retained = admitted;
            let element = std::mem::ManuallyDrop::new(unsafe {
                IUIAutomationElement::from_raw(retained.as_ptr() as *mut _)
            });
            let mut target = UiaControl {
                element: &element,
                hwnd: input.window_id,
                pid: input.pid,
                prepared: None,
            };
            match semantic::execute(&mut target, input.operation, input.value.as_deref()) {
                Ok(pattern) => ToolResult::text("UIA pattern completed; verify application state and focus independently.")
                    .with_structured(json!({"operation":input.operation,"pattern":pattern,"route":"accessibility","effect":"unverifiable","delivery":{"mode":"background"}})),
                Err(error) => failure(error),
            }
        }).await;
        match result {
            Ok(result) => result,
            Err(_) => failure(Failure {
                code: "semantic_worker_failed",
                attempted: true,
                hresult: None,
            }),
        }
    }
}
