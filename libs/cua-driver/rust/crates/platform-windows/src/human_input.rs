//! Human takeover for foreground input.
//!
//! When `CUA_HUMAN_IDLE_MS` holds a positive number at startup, low-level keyboard and mouse hooks record
//! when the last physical event arrived: one without the injected flag that SendInput, touch and pen
//! injection always set. Foreground SendInput then refuses until that many milliseconds pass without
//! physical input, so the agent never fights the person at the mouse and keyboard. Background delivery
//! (PostMessage, UIA, touch injection) does not move their pointer or focus and is never paused.
//! Releases of keys and buttons the agent holds always go through.
//!
//! Without the variable no hook is installed and nothing changes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static IDLE_MS: OnceLock<u64> = OnceLock::new();
/// `GetTickCount64` of the last physical event; 0 when none was seen.
static LAST_PHYSICAL: AtomicU64 = AtomicU64::new(0);

fn idle_ms() -> u64 {
    *IDLE_MS.get_or_init(|| {
        std::env::var("CUA_HUMAN_IDLE_MS")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// Install the hooks on their own message-loop thread when the takeover pause is configured. Idempotent.
pub fn start_from_env() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if idle_ms() == 0 {
        return;
    }
    STARTED.get_or_init(|| {
        #[cfg(target_os = "windows")]
        if let Err(error) = std::thread::Builder::new()
            .name("cua-human-input".into())
            .spawn(hooks::run)
        {
            tracing::warn!("human input hooks did not start: {error}");
        }
    });
}

/// Milliseconds since the last physical input, when one was seen since startup.
pub fn physical_idle_ms() -> Option<u64> {
    let last = LAST_PHYSICAL.load(Ordering::Acquire);
    (last != 0).then(|| now().saturating_sub(last))
}

/// Refuse foreground input while the human used the keyboard or mouse within the configured pause.
pub fn check() -> Result<(), String> {
    let idle = idle_ms();
    match physical_idle_ms() {
        Some(age) if idle > 0 && age < idle => Err(format!(
            "human_takeover: the human used the keyboard or mouse {age} ms ago; foreground input \
             waits until {idle} ms pass without physical input (retry in {} ms). Background \
             delivery is not paused.",
            idle - age
        )),
        _ => Ok(()),
    }
}

#[cfg(target_os = "windows")]
fn now() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

#[cfg(not(target_os = "windows"))]
fn now() -> u64 {
    0
}

#[cfg(target_os = "windows")]
mod hooks {
    use super::{now, LAST_PHYSICAL};
    use std::sync::atomic::Ordering;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL,
    };

    unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if (event.flags & LLKHF_INJECTED).0 == 0 {
                LAST_PHYSICAL.store(now(), Ordering::Release);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if event.flags & LLMHF_INJECTED == 0 {
                LAST_PHYSICAL.store(now(), Ordering::Release);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Low-level hooks run on the installing thread's message loop; the callbacks only store a tick.
    pub(super) fn run() {
        unsafe {
            let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), None, 0);
            let pointer = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), None, 0);
            if keys.is_err() || pointer.is_err() {
                tracing::warn!("human input hooks: keyboard {keys:?}, mouse {pointer:?}");
            }
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {}
        }
    }
}
