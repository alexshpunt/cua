//! Triple Escape stops the agent.
//!
//! When `CUA_ESCAPE_STOP=1` is set at startup, a low-level keyboard hook watches for the person at the
//! machine pressing Escape three times within a second. Only physical presses count: SendInput and
//! every other injection set the injected flag, so an agent's own Escape never stops it, and a held
//! key's auto-repeat counts once. The third press calls [`cua_driver_core::human_stop::trigger`]: this
//! process refuses all further input and running input stops at its next step.
//!
//! Agent input does not wait for the human: it takes the foreground as soon as it runs.

use std::time::Duration;

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

/// Install the hook on its own message-loop thread when the stop is configured. Idempotent.
pub fn start_from_env() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if std::env::var("CUA_ESCAPE_STOP").as_deref() != Ok("1") {
        return;
    }
    STARTED.get_or_init(|| {
        #[cfg(target_os = "windows")]
        if let Err(error) = std::thread::Builder::new()
            .name("cua-escape-stop".into())
            .spawn(hooks::run)
        {
            tracing::warn!("escape stop hook did not start: {error}");
        }
    });
}

/// Refuse input, or stop running input at its next step, once the human pressed Escape three times.
pub fn check() -> Result<(), String> {
    cua_driver_core::human_stop::check()
}

#[cfg(target_os = "windows")]
mod hooks {
    use super::Detector;
    use std::sync::Mutex;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        LLKHF_UP, MSG, WH_KEYBOARD_LL,
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
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Low-level hooks run on the installing thread's message loop; the callback does no I/O.
    pub(super) fn run() {
        unsafe {
            if let Err(error) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), None, 0) {
                tracing::warn!("escape stop hook: {error:?}");
                return;
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
