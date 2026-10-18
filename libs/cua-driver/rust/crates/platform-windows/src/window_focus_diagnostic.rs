//! Single-attempt native focus observation, without input or an activation fallback.

use serde::Serialize;

/// Numeric native identities. A zero thread ID means Windows supplied no thread identity.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct FocusSource {
    pub current_thread_id: u32,
    pub foreground_thread_id: u32,
    pub target_thread_id: u32,
    pub foreground_window: u64,
    pub target_window: u64,
}

/// One API return; `error` is sampled immediately and only after an attachment failure.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct AttachmentReturn {
    pub succeeded: bool,
    pub error: Option<u32>,
}

/// Bounded, text-free observations, not an explanation of Windows foreground eligibility.
/// Null API returns mean the corresponding call was not attempted.
#[derive(Debug, Serialize)]
pub(crate) struct FocusDiagnostic {
    pub format: &'static str,
    pub cause: &'static str,
    pub source: FocusSource,
    pub attachment_requested: bool,
    pub attachment_return: Option<AttachmentReturn>,
    /// This request's attach returned success, not exclusive queue ownership or a foreground grant.
    pub attachment_owned: bool,
    pub activation_return: Option<bool>,
    pub foreground_before_detach: u64,
    pub detachment_requested: bool,
    pub detachment_return: Option<AttachmentReturn>,
    pub foreground_after_detach: u64,
}

/// Production seam for the existing native-only focus algorithm. No input API is available here.
pub(crate) trait FocusApi {
    fn source(&mut self) -> FocusSource;
    fn attach(&mut self, current: u32, foreground: u32, attach: bool) -> AttachmentReturn;
    fn activate(&mut self, target: u64) -> bool;
    fn foreground(&mut self) -> u64;
}

/// Preserve the original call order and final-HWND criterion, including the original
/// detach request after a failed attach. Ownership records actual attach success,
/// not merely the request; no retry, corrective detach or fallback is introduced.
pub(crate) fn attempt(api: &mut impl FocusApi) -> (bool, FocusDiagnostic) {
    let source = api.source();
    let mut diagnostic = FocusDiagnostic {
        format: "native_focus_api_v1",
        cause: "not_determined",
        source,
        attachment_requested: false,
        attachment_return: None,
        attachment_owned: false,
        activation_return: None,
        foreground_before_detach: source.foreground_window,
        detachment_requested: false,
        detachment_return: None,
        foreground_after_detach: source.foreground_window,
    };
    if source.foreground_window == source.target_window {
        return (true, diagnostic);
    }
    diagnostic.attachment_requested =
        source.foreground_thread_id != 0 && source.foreground_thread_id != source.current_thread_id;
    if diagnostic.attachment_requested {
        let result = api.attach(source.current_thread_id, source.foreground_thread_id, true);
        diagnostic.attachment_owned = result.succeeded;
        diagnostic.attachment_return = Some(result);
    }
    diagnostic.activation_return = Some(api.activate(source.target_window));
    diagnostic.foreground_before_detach = api.foreground();
    if diagnostic.attachment_requested {
        diagnostic.detachment_requested = true;
        diagnostic.detachment_return =
            Some(api.attach(source.current_thread_id, source.foreground_thread_id, false));
    }
    diagnostic.foreground_after_detach = api.foreground();
    (
        diagnostic.foreground_after_detach == source.target_window,
        diagnostic,
    )
}

#[cfg(windows)]
struct NativeApi(windows::Win32::Foundation::HWND);

#[cfg(windows)]
impl FocusApi for NativeApi {
    fn source(&mut self) -> FocusSource {
        use windows::Win32::{
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        };
        unsafe {
            let foreground = GetForegroundWindow();
            FocusSource {
                current_thread_id: GetCurrentThreadId(),
                foreground_thread_id: GetWindowThreadProcessId(foreground, None),
                target_thread_id: GetWindowThreadProcessId(self.0, None),
                foreground_window: foreground.0 as usize as u64,
                target_window: self.0 .0 as usize as u64,
            }
        }
    }
    fn attach(&mut self, current: u32, foreground: u32, attach: bool) -> AttachmentReturn {
        use windows::Win32::{Foundation::GetLastError, System::Threading::AttachThreadInput};
        unsafe {
            let succeeded = AttachThreadInput(current, foreground, attach).as_bool();
            // AttachThreadInput documents GetLastError on failure. Read before any other API.
            let error = if succeeded {
                None
            } else {
                Some(GetLastError().0)
            };
            AttachmentReturn { succeeded, error }
        }
    }
    fn activate(&mut self, _target: u64) -> bool {
        // SetForegroundWindow does not document an extended GetLastError result.
        unsafe { windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(self.0).as_bool() }
    }
    fn foreground(&mut self) -> u64 {
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as usize as u64 }
    }
}

/// Observe the single native management focus request; callers retain existing lifetime,
/// desktop and cancellation admission. Never use this as an input-activation fallback.
#[cfg(windows)]
pub(crate) fn focus(target: windows::Win32::Foundation::HWND) -> (bool, FocusDiagnostic) {
    attempt(&mut NativeApi(target))
}
