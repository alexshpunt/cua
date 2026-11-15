//! The person at the machine: triple Escape stops the agent, and physical presses are counted.
//!
//! When `CUA_ESCAPE_STOP=1` is set at startup, a low-level keyboard hook watches for the person at the
//! machine pressing Escape three times within a second. Only physical presses count: SendInput and
//! every other injection set the injected flag, so an agent's own Escape never stops it, and a held
//! key's auto-repeat counts once. The third press calls [`cua_driver_core::human_stop::trigger`]: this
//! process refuses all further input and running input stops at its next step.
//!
//! When `CUA_FOCUS_RETURN=1` is set, the same hook thread also counts every physical key press and
//! mouse button press, so [`crate::focus_return`] can tell whether the human used the computer since
//! the agent's last action.
//!
//! Agent input does not wait for the human: it takes the foreground as soon as it runs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Physical key and mouse button presses since the hooks started.
static PHYSICAL_PRESSES: AtomicU64 = AtomicU64::new(0);
/// The hooks are installed and counting.
static COUNTING: AtomicBool = AtomicBool::new(false);
static ESCAPE_STOP: AtomicBool = AtomicBool::new(false);

/// How many physical key and mouse button presses the hooks have seen, or `None` when no hook
/// counts them: focus return was not configured or Windows refused the hooks.
pub(crate) fn physical_presses() -> Option<u64> {
    COUNTING
        .load(Ordering::Acquire)
        .then(|| PHYSICAL_PRESSES.load(Ordering::Acquire))
}

/// Whether `CUA_FOCUS_RETURN=1` asked agent input to hand the foreground back to the human.
pub(crate) fn focus_return_configured() -> bool {
    std::env::var("CUA_FOCUS_RETURN").as_deref() == Ok("1")
}

/// Presses within this window, first to last, stop the agent.
const WINDOW: Duration = Duration::from_millis(1000);
const PRESSES: usize = 3;

/// Counts physical Escape presses; a pure state machine, so it is testable off Windows.
#[derive(Default)]
struct Detector {
    down: bool,
    presses: [u64; PRESSES],
    count: usize,
}

impl Detector {
    /// Feed one physical Escape transition at `now_ms`; true when this press completes the sequence.
    fn escape(&mut self, pressed: bool, now_ms: u64) -> bool {
        if !pressed {
            self.down = false;
            return false;
        }
        if self.down {
            // Auto-repeat of a held key.
            return false;
        }
        self.down = true;
        if self.count == PRESSES {
            self.presses.rotate_left(1);
            self.count -= 1;
        }
        self.presses[self.count] = now_ms;
        self.count += 1;
        if self.count == PRESSES
            && now_ms.saturating_sub(self.presses[0]) <= WINDOW.as_millis() as u64
        {
            self.count = 0;
            return true;
        }
        false
    }

    /// Any other physical key breaks the sequence.
    fn other_key(&mut self) {
        self.count = 0;
    }
}

/// Install the hooks on their own message-loop thread when the stop or focus return is configured.
/// Idempotent.
pub fn start_from_env() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let escape_stop = std::env::var("CUA_ESCAPE_STOP").as_deref() == Ok("1");
    let count_presses = focus_return_configured();
    if !escape_stop && !count_presses {
        return;
    }
    STARTED.get_or_init(|| {
        ESCAPE_STOP.store(escape_stop, Ordering::Release);
        #[cfg(target_os = "windows")]
        if let Err(error) = std::thread::Builder::new()
            .name("cua-human-input".into())
            .spawn(move || hooks::run(count_presses))
        {
            tracing::warn!("human input hooks did not start: {error}");
        }
    });
}

/// Refuse input, or stop running input at its next step, once the human pressed Escape three times.
pub fn check() -> Result<(), String> {
    cua_driver_core::human_stop::check()
}

#[cfg(target_os = "windows")]
mod hooks {
    use super::{Detector, COUNTING, ESCAPE_STOP, PHYSICAL_PRESSES};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        LLKHF_UP, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN,
        WM_MBUTTONDOWN, WM_RBUTTONDOWN, WM_XBUTTONDOWN,
    };

    static DETECTOR: Mutex<Detector> = Mutex::new(Detector {
        down: false,
        presses: [0; super::PRESSES],
        count: 0,
    });

    unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if (event.flags & LLKHF_INJECTED).0 == 0 {
                let pressed = (event.flags & LLKHF_UP).0 == 0;
                if pressed {
                    PHYSICAL_PRESSES.fetch_add(1, Ordering::AcqRel);
                }
                if ESCAPE_STOP.load(Ordering::Acquire) {
                    if let Ok(mut detector) = DETECTOR.lock() {
                        if event.vkCode == u32::from(VK_ESCAPE.0) {
                            // The event's own tick, so a slow hook thread cannot stretch the window.
                            if detector.escape(pressed, u64::from(event.time)) {
                                cua_driver_core::human_stop::trigger();
                            }
                        } else if pressed {
                            detector.other_key();
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let press = matches!(
                wparam.0 as u32,
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
            );
            if press && event.flags & LLMHF_INJECTED == 0 {
                PHYSICAL_PRESSES.fetch_add(1, Ordering::AcqRel);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Low-level hooks run on the installing thread's message loop; the callbacks do no I/O.
    pub(super) fn run(count_presses: bool) {
        unsafe {
            if let Err(error) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), None, 0) {
                tracing::warn!("human input keyboard hook: {error:?}");
                return;
            }
            if count_presses {
                match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), None, 0) {
                    Ok(_) => COUNTING.store(true, Ordering::Release),
                    Err(error) => tracing::warn!("human input mouse hook: {error:?}"),
                }
            }
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Detector;

    #[test]
    fn three_presses_within_a_second_stop() {
        let mut d = Detector::default();
        assert!(!d.escape(true, 100));
        assert!(!d.escape(false, 150));
        assert!(!d.escape(true, 400));
        assert!(!d.escape(false, 450));
        assert!(d.escape(true, 1100));
    }

    #[test]
    fn auto_repeat_counts_once() {
        let mut d = Detector::default();
        for t in [0, 30, 60, 90, 120] {
            assert!(!d.escape(true, t));
        }
        assert!(!d.escape(false, 200));
        assert!(!d.escape(true, 300));
        d.escape(false, 350);
        assert!(d.escape(true, 400));
    }

    #[test]
    fn slow_presses_slide_the_window() {
        let mut d = Detector::default();
        for t in [0, 600, 1200] {
            assert!(!d.escape(true, t));
            d.escape(false, t + 50);
        }
        // 600, 1200 and 1500 fit in one second.
        assert!(d.escape(true, 1500));
    }

    #[test]
    fn another_key_breaks_the_sequence() {
        let mut d = Detector::default();
        d.escape(true, 0);
        d.escape(false, 10);
        d.escape(true, 100);
        d.escape(false, 110);
        d.other_key();
        assert!(!d.escape(true, 200));
        d.escape(false, 210);
        assert!(!d.escape(true, 300));
        d.escape(false, 310);
        assert!(d.escape(true, 400));
    }
}
