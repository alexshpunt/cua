// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Agent input hands the foreground back to the human's window.
//!
//! With `CUA_FOCUS_RETURN=1`, a foreground input action gives the foreground back only when
//! supported receiver-read evidence covers every inserted event and no earlier unconfirmed
//! prefix remains. The window that gets it
//! is the human's: the window that had the foreground when the human last pressed a key or a mouse
//! button, as the low-level hooks of [`crate::human_input`] count them. It is not simply the window
//! that was in front before the action: that one may be the agent's own window from an earlier
//! action, and handing the foreground to it brought unrelated windows forward.
//!
//! The foreground stays where it is when:
//! - the human's window already has it, for example when the human used the target last;
//! - a window that is not the agent's took it during the action, such as one the human clicked;
//! - a menu, drop-down or other captionless popup of the target's process is open that the action
//!   opened, or that an earlier action opened and kept: activating another window would close it
//!   and break the agent's next step;
//! - the human's window is closed, hidden, minimized or on another virtual desktop.
//!
//! The decision is pure ([`plan`], [`open_popup`], [`human_at_start`]) so it is testable off Windows.

/// A top-level window and the process that owned it when it was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub hwnd: u64,
    pub pid: u32,
}

/// What one driver process remembers between its actions.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// The human's window as of the end of the last action.
    human: Option<Window>,
    /// The physical press count read at the end of the last action.
    presses_seen: Option<u64>,
    /// Popups of the agent's target that the last action kept open.
    kept_popups: Vec<u64>,
    /// An earlier global-input prefix has no supported read witness. Only a
    /// subsequent physical press explicitly hands control back to the human;
    /// a later unrelated key receipt cannot acknowledge this prefix.
    unconfirmed_since: Option<Option<u64>>,
}

/// Whether an earlier unconfirmed prefix still forbids automatic restoration.
fn has_unconfirmed_prefix(memory: &Memory, presses: Option<u64>) -> bool {
    match memory.unconfirmed_since {
        None => false,
        Some(Some(before)) if presses.is_some_and(|now| now != before) => false,
        Some(_) => true,
    }
}
/// The human's window at the start of an agent action.
///
/// When the human pressed a key or a mouse button since the last action ended, or when nothing is
/// remembered yet, the window in front is the human's. Otherwise the human did not touch the
/// computer, and the window in front may be the agent's own; the remembered window stays the
/// human's. Without the hooks (`presses` is `None`) the window in front is taken.
pub(crate) fn human_at_start(
    memory: &Memory,
    presses: Option<u64>,
    foreground: Option<Window>,
) -> Option<Window> {
    match (presses, memory.human) {
        (Some(now), Some(human)) if memory.presses_seen == Some(now) => Some(human),
        _ => foreground.or(memory.human),
    }
}

/// The first popup of the target that the action opened, or one an earlier action kept open and
/// that is still open. `before` and `after` list the target's open popups at the start and the end.
pub(crate) fn open_popup(before: &[u64], kept: &[u64], after: &[u64]) -> Option<u64> {
    after
        .iter()
        .copied()
        .find(|popup| kept.contains(popup) || !before.contains(popup))
}

/// Facts at the end of an action.
#[derive(Clone, Copy, Debug)]
pub(crate) struct End {
    pub foreground: Option<Window>,
    pub human: Option<Window>,
    /// The human pressed a key or a mouse button while the action ran.
    pub human_pressed: bool,
    /// The window in front is the target, a window it owns or embeds, or another window its
    /// process put in front.
    pub foreground_is_agents: bool,
    pub popup: Option<u64>,
    /// The human's window still exists, is visible, not minimized and on the current desktop.
    pub human_usable: bool,
    /// Every inserted global input event has a supported receiver-read witness,
    /// or no global input was inserted.
    pub input_read_confirmed: bool,
}

/// What happened to the foreground after an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The human's window got the foreground back.
    Restored(u64),
    /// Windows kept another window in front.
    Refused { human: u64, foreground: u64 },
    /// The human's window already has the foreground.
    AlreadyHuman,
    /// The human switched to this window while the action ran; it stays in front.
    HumanMoved(u64),
    /// A window that is not the agent's took the foreground; it stays in front.
    OtherWindow(u64),
    /// This popup of the target is open; the target keeps the foreground.
    KeptForPopup(u64),
    /// Read knowledge does not permit handing queued input to the human.
    KeptForUnconfirmedRead(u64),
    /// The human's window is closed, hidden, minimized or on another desktop.
    HumanWindowGone(u64),
    /// No window of the human is known.
    NoHumanWindow,
    /// No window has the foreground.
    NoForeground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Restore(Window),
    Leave(Outcome),
}

pub(crate) fn plan(end: &End) -> Plan {
    let Some(foreground) = end.foreground else {
        return Plan::Leave(Outcome::NoForeground);
    };
    if end.human.map(|human| human.hwnd) == Some(foreground.hwnd) {
        return Plan::Leave(Outcome::AlreadyHuman);
    }
    if !end.foreground_is_agents {
        return Plan::Leave(if end.human_pressed {
            Outcome::HumanMoved(foreground.hwnd)
        } else {
            Outcome::OtherWindow(foreground.hwnd)
        });
    }
    if let Some(popup) = end.popup {
        return Plan::Leave(Outcome::KeptForPopup(popup));
    }
    let Some(human) = end.human else {
        return Plan::Leave(Outcome::NoHumanWindow);
    };
    if !end.human_usable {
        return Plan::Leave(Outcome::HumanWindowGone(human.hwnd));
    }
    if !end.input_read_confirmed {
        return Plan::Leave(Outcome::KeptForUnconfirmedRead(foreground.hwnd));
    }
    Plan::Restore(human)
}

/// Update the memory once an action is over. `presses` is read after any restore.
pub(crate) fn remember(
    memory: &mut Memory,
    human: Option<Window>,
    foreground: Option<Window>,
    outcome: Outcome,
    presses: Option<u64>,
    kept: Vec<u64>,
) {
    memory.human = match outcome {
        Outcome::HumanMoved(_) => foreground,
        _ => human,
    };
    memory.presses_seen = presses;
    memory.kept_popups = match outcome {
        Outcome::KeptForPopup(_) => kept,
        _ => Vec::new(),
    };
}

/// One line for the action's result, starting with `focus_return:`, or none when nothing changed
/// that the agent needs to know.
pub(crate) fn describe(outcome: Outcome, title: impl Fn(u64) -> String) -> Option<String> {
    let name = |hwnd: u64| serde_json::to_string(&title(hwnd)).unwrap_or_else(|_| "\"\"".into());
    Some(match outcome {
        Outcome::Restored(hwnd) => format!(
            "focus_return: restored window_id={hwnd} title={}; the human's window is in front again.",
            name(hwnd)
        ),
        Outcome::Refused { human, foreground } => format!(
            "focus_return: refused window_id={human} title={}; Windows kept window_id={foreground} in front.",
            name(human)
        ),
        Outcome::KeptForPopup(popup) => format!(
            "focus_return: kept_for_popup window_id={popup}; a menu or popup of the target is open, so the target keeps the foreground until an action closes it."
        ),
        Outcome::KeptForUnconfirmedRead(hwnd) => format!(
            "focus_return: kept_for_unconfirmed_read window_id={hwnd}; no supported witness confirms every inserted event was read, so the target keeps the foreground. Nothing was replayed."
        ),
        Outcome::HumanWindowGone(hwnd) => format!(
            "focus_return: human_window_gone window_id={hwnd}; the human's last window is closed, hidden, minimized or on another desktop, so the target keeps the foreground."
        ),
        Outcome::HumanMoved(hwnd) => format!(
            "focus_return: human_moved window_id={hwnd} title={}; the human switched to this window during the action.",
            name(hwnd)
        ),
        Outcome::OtherWindow(hwnd) => format!(
            "focus_return: other_window window_id={hwnd} title={}; this window took the foreground during the action and keeps it.",
            name(hwnd)
        ),
        Outcome::AlreadyHuman | Outcome::NoHumanWindow | Outcome::NoForeground => return None,
    })
}

#[cfg(target_os = "windows")]
pub(crate) use native::{
    active, begin, enabled, has_unconfirmed_input, note_unconfirmed_input, unconfirmed_generation,
};

#[cfg(target_os = "windows")]
mod native {
    use super::{
        describe, has_unconfirmed_prefix, human_at_start, open_popup, plan, remember, End, Memory,
        Outcome, Plan, Window,
    };
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
        IsIconic, IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, WS_CAPTION, WS_EX_TRANSPARENT,
    };

    static MEMORY: Mutex<Memory> = Mutex::new(Memory {
        human: None,
        presses_seen: None,
        kept_popups: Vec::new(),
        unconfirmed_since: None,
    });

    static UNCONFIRMED_GENERATION: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn unconfirmed_generation() -> u64 {
        UNCONFIRMED_GENERATION.load(Ordering::Acquire)
    }
    pub(crate) fn has_unconfirmed_input() -> bool {
        let presses = crate::human_input::physical_presses();
        let mut memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
        if !has_unconfirmed_prefix(&memory, presses) {
            memory.unconfirmed_since = None;
        }
        memory.unconfirmed_since.is_some()
    }

    pub(crate) fn note_unconfirmed_input() {
        UNCONFIRMED_GENERATION.fetch_add(1, Ordering::AcqRel);
        let presses = crate::human_input::physical_presses();
        let mut memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
        if !has_unconfirmed_prefix(&memory, presses) {
            memory.unconfirmed_since = Some(presses);
        }
    }
    /// Leases that have begun and are not over yet.
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);

    /// How long a popup the action opened may take to appear before the foreground goes back.
    const POPUP_SETTLE: Duration = Duration::from_millis(150);

    /// Focus return is on: `CUA_FOCUS_RETURN=1` at startup.
    pub(crate) fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(crate::human_input::focus_return_configured)
    }

    /// A lease owns the conditional return to the remembered human window.
    /// Input without a lease still obeys the same unconfirmed-prefix guard.
    pub(crate) fn active() -> bool {
        ACTIVE.load(Ordering::Acquire) > 0
    }

    fn window(hwnd: HWND) -> Option<Window> {
        let hwnd = hwnd.0 as usize as u64;
        (hwnd != 0)
            .then(|| crate::win32::window_owner_pid(hwnd).map(|pid| Window { hwnd, pid }))
            .flatten()
    }

    fn cloaked(hwnd: HWND) -> bool {
        let mut cloaked = 0u32;
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            )
        }
        .is_ok()
            && cloaked != 0
    }

    fn usable(human: Window) -> bool {
        let hwnd = HWND(human.hwnd as usize as *mut _);
        crate::win32::window_owner_pid(human.hwnd) == Some(human.pid)
            && unsafe { IsWindowVisible(hwnd) }.as_bool()
            && !unsafe { IsIconic(hwnd) }.as_bool()
            && !cloaked(hwnd)
    }

    fn class_name(hwnd: HWND) -> String {
        let mut buffer = [0u16; 64];
        let length = unsafe { GetClassNameW(hwnd, &mut buffer) }.max(0) as usize;
        String::from_utf16_lossy(&buffer[..length.min(buffer.len())])
    }

    /// A visible captionless top-level window, such as a menu or a drop-down list. Tooltips, input
    /// method windows and click-through overlays do not count.
    fn is_popup(hwnd: HWND) -> bool {
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut rect = RECT::default();
        unsafe { IsWindowVisible(hwnd) }.as_bool()
            && style & WS_CAPTION.0 != WS_CAPTION.0
            && ex & WS_EX_TRANSPARENT.0 == 0
            && unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
            && rect.right > rect.left
            && rect.bottom > rect.top
            && !cloaked(hwnd)
            && !matches!(
                class_name(hwnd).as_str(),
                "tooltips_class32" | "IME" | "MSCTFIME UI"
            )
    }

    struct Search {
        pid: u32,
        target: u64,
        found: Vec<u64>,
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let id = hwnd.0 as usize as u64;
        if id != search.target
            && crate::win32::window_owner_pid(id) == Some(search.pid)
            && is_popup(hwnd)
        {
            search.found.push(id);
        }
        BOOL(1)
    }

    /// The open popups of the target's process.
    fn popups(target: u64, pid: u32) -> Vec<u64> {
        let mut search = Search {
            pid,
            target,
            found: Vec::new(),
        };
        let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut search as *mut Search as isize)) };
        search.found
    }

    /// The foreground state at the start of one foreground action on `target`.
    pub(crate) struct Lease {
        target: u64,
        pid: u32,
        snapshot: crate::win32::ForegroundTarget,
        human: Option<Window>,
        presses: Option<u64>,
        popups_before: Vec<u64>,
        kept_before: Vec<u64>,
        finished: bool,
    }

    /// Start a lease before a foreground action on the exact `target` window, or `None` when focus
    /// return is off or the target is gone.
    pub(crate) fn begin(target: u64) -> Option<Lease> {
        if !enabled() {
            return None;
        }
        let pid = crate::win32::window_owner_pid(target)?;
        let snapshot = crate::win32::capture_foreground_target(target)?;
        let presses = crate::human_input::physical_presses();
        let foreground = window(unsafe { GetForegroundWindow() });
        let memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
        ACTIVE.fetch_add(1, Ordering::AcqRel);
        Some(Lease {
            target,
            pid,
            snapshot,
            human: human_at_start(&memory, presses, foreground),
            presses,
            popups_before: popups(target, pid),
            kept_before: memory.kept_popups.clone(),
            finished: false,
        })
    }

    impl Drop for Lease {
        fn drop(&mut self) {
            if !self.finished {
                // Caller cancellation may leave a blocking input worker alive.
                // Retain the human and uncertainty, but never restore from Drop.
                note_unconfirmed_input();
                let presses = crate::human_input::physical_presses();
                let foreground = window(unsafe { GetForegroundWindow() });
                let moved = matches!((self.presses, presses), (Some(a), Some(b)) if a != b);
                let outcome = match foreground {
                    Some(front) if moved && !self.agents(front, true) => {
                        Outcome::HumanMoved(front.hwnd)
                    }
                    _ => Outcome::KeptForUnconfirmedRead(self.target),
                };
                let mut memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
                remember(
                    &mut memory,
                    self.human,
                    foreground,
                    outcome,
                    presses,
                    Vec::new(),
                );
            }
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }

    impl Lease {
        fn agents(&self, foreground: Window, human_pressed: bool) -> bool {
            foreground.hwnd == self.target
                || crate::win32::foreground_matches_target_or_owned_window(
                    self.snapshot,
                    foreground.hwnd,
                )
                || crate::win32::foreground_after_target_closed(self.snapshot, foreground.hwnd)
                // Another window of the target's app, such as one the action opened. When the human
                // pressed something meanwhile, it may be the human's own window of the same app.
                || (foreground.pid == self.pid && !human_pressed)
        }

        /// Hand the foreground back when the action is over, and return the line for its result.
        pub(crate) fn finish(mut self, input_read_confirmed: bool) -> Option<String> {
            let presses = crate::human_input::physical_presses;
            let started = self.presses;
            let human_pressed =
                |now: Option<u64>| matches!((started, now), (Some(a), Some(b)) if a != b);
            let mut foreground = window(unsafe { GetForegroundWindow() });
            let mut popup = None;
            let mut open = Vec::new();
            // A popup the action opened can appear a little after the input; wait for it only
            // while the agent's window is in front.
            let deadline = Instant::now() + POPUP_SETTLE;
            loop {
                let agents_front = foreground
                    .is_some_and(|front| self.agents(front, human_pressed(presses())))
                    && foreground.map(|front| front.hwnd) != self.human.map(|human| human.hwnd);
                if !agents_front {
                    break;
                }
                open = popups(self.target, self.pid);
                popup = open_popup(&self.popups_before, &self.kept_before, &open);
                // A popup already present at entry may still be closing while
                // the queued key is processed. Give that transition the same
                // bounded UI-settle window; it is not an input-read receipt.
                let newly_opened = popup.is_some_and(|hwnd| !self.popups_before.contains(&hwnd));
                if newly_opened || Instant::now() >= deadline {
                    break;
                }
                sleep(Duration::from_millis(15));
                foreground = window(unsafe { GetForegroundWindow() });
            }
            let pressed = human_pressed(presses());
            let end = End {
                foreground,
                human: self.human,
                human_pressed: pressed,
                foreground_is_agents: foreground.is_some_and(|front| self.agents(front, pressed)),
                popup,
                human_usable: self.human.is_some_and(usable),
                input_read_confirmed: input_read_confirmed && !has_unconfirmed_input(),
            };
            let outcome = match plan(&end) {
                Plan::Leave(outcome) => outcome,
                Plan::Restore(human) => {
                    let hwnd = HWND(human.hwnd as usize as *mut _);
                    // Returning focus must not create another unconfirmed key
                    // prefix through the explicit-activation unlock fallback.
                    if unsafe { crate::input::force_foreground_attached(hwnd) } {
                        Outcome::Restored(human.hwnd)
                    } else {
                        Outcome::Refused {
                            human: human.hwnd,
                            foreground: unsafe { GetForegroundWindow() }.0 as usize as u64,
                        }
                    }
                }
            };
            let kept = open
                .into_iter()
                .filter(|window| {
                    self.kept_before.contains(window) || !self.popups_before.contains(window)
                })
                .collect();
            let mut memory = MEMORY.lock().unwrap_or_else(|poison| poison.into_inner());
            remember(
                &mut memory,
                self.human,
                foreground,
                outcome,
                presses(),
                kept,
            );
            drop(memory);
            self.finished = true;
            tracing::debug!(target: "focus_return", ?outcome, "foreground after agent input");
            describe(outcome, crate::input::occlusion::window_title)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUMAN: Window = Window { hwnd: 1, pid: 10 };
    const TARGET: Window = Window { hwnd: 2, pid: 20 };

    fn base() -> End {
        End {
            foreground: Some(TARGET),
            human: Some(HUMAN),
            human_pressed: false,
            foreground_is_agents: true,
            popup: None,
            human_usable: true,
            input_read_confirmed: true,
        }
    }

    #[test]
    fn the_human_window_gets_the_foreground_back() {
        assert_eq!(plan(&base()), Plan::Restore(HUMAN));
    }

    #[test]
    fn unconfirmed_input_keeps_the_target_and_the_original_human_memory() {
        let end = End {
            input_read_confirmed: false,
            ..base()
        };
        let outcome = Outcome::KeptForUnconfirmedRead(TARGET.hwnd);
        assert_eq!(plan(&end), Plan::Leave(outcome));
        let mut memory = Memory {
            unconfirmed_since: Some(Some(5)),
            ..Memory::default()
        };
        remember(
            &mut memory,
            end.human,
            end.foreground,
            outcome,
            Some(5),
            Vec::new(),
        );
        assert_eq!(human_at_start(&memory, Some(5), Some(TARGET)), Some(HUMAN));
        assert!(has_unconfirmed_prefix(&memory, Some(5)));
        assert!(has_unconfirmed_prefix(&memory, None));
        assert!(!has_unconfirmed_prefix(&memory, Some(6)));
    }
    #[test]
    fn nothing_changes_when_the_human_window_is_in_front() {
        let end = End {
            foreground: Some(HUMAN),
            ..base()
        };
        assert_eq!(plan(&end), Plan::Leave(Outcome::AlreadyHuman));
    }

    #[test]
    fn a_window_the_human_switched_to_keeps_the_foreground() {
        let other = Window { hwnd: 3, pid: 30 };
        let moved = End {
            foreground: Some(other),
            foreground_is_agents: false,
            human_pressed: true,
            ..base()
        };
        assert_eq!(plan(&moved), Plan::Leave(Outcome::HumanMoved(3)));
        let stolen = End {
            human_pressed: false,
            ..moved
        };
        assert_eq!(plan(&stolen), Plan::Leave(Outcome::OtherWindow(3)));
    }

    #[test]
    fn an_open_popup_keeps_the_foreground_with_the_target() {
        let end = End {
            popup: Some(7),
            ..base()
        };
        assert_eq!(plan(&end), Plan::Leave(Outcome::KeptForPopup(7)));
    }

    #[test]
    fn a_gone_human_window_is_not_activated() {
        let end = End {
            human_usable: false,
            ..base()
        };
        assert_eq!(plan(&end), Plan::Leave(Outcome::HumanWindowGone(1)));
        let unknown = End {
            human: None,
            ..base()
        };
        assert_eq!(plan(&unknown), Plan::Leave(Outcome::NoHumanWindow));
    }

    #[test]
    fn without_presses_since_the_last_action_the_remembered_window_stays_the_human_s() {
        let mut memory = Memory::default();
        // First action: the window in front is the human's.
        assert_eq!(human_at_start(&memory, Some(5), Some(HUMAN)), Some(HUMAN));
        remember(
            &mut memory,
            Some(HUMAN),
            Some(TARGET),
            Outcome::KeptForPopup(7),
            Some(5),
            vec![7],
        );
        // The agent's window is still in front because of its popup; the human did nothing.
        assert_eq!(human_at_start(&memory, Some(5), Some(TARGET)), Some(HUMAN));
        // The human pressed something: the window in front is theirs now.
        assert_eq!(human_at_start(&memory, Some(6), Some(TARGET)), Some(TARGET));
        // Without hooks, the window in front is taken.
        assert_eq!(human_at_start(&memory, None, Some(TARGET)), Some(TARGET));
    }

    #[test]
    fn a_human_switch_during_the_action_becomes_the_human_window() {
        let mut memory = Memory::default();
        let other = Window { hwnd: 3, pid: 30 };
        remember(
            &mut memory,
            Some(HUMAN),
            Some(other),
            Outcome::HumanMoved(3),
            Some(9),
            Vec::new(),
        );
        assert_eq!(human_at_start(&memory, Some(9), Some(TARGET)), Some(other));
    }

    #[test]
    fn only_popups_the_agent_opened_or_kept_count() {
        // A captionless window that was already open, such as another window of the app, does not.
        assert_eq!(open_popup(&[5], &[], &[5]), None);
        assert_eq!(open_popup(&[5], &[], &[5, 6]), Some(6));
        // A menu an earlier action opened and kept still counts at the next action.
        assert_eq!(open_popup(&[6], &[6], &[6]), Some(6));
        assert_eq!(open_popup(&[6], &[6], &[]), None);
    }

    #[test]
    fn the_result_line_names_what_happened() {
        let title = |_| "Notepad".to_owned();
        assert_eq!(
            describe(Outcome::Restored(1), title).unwrap(),
            "focus_return: restored window_id=1 title=\"Notepad\"; the human's window is in front again."
        );
        assert!(describe(Outcome::KeptForPopup(7), title)
            .unwrap()
            .starts_with("focus_return: kept_for_popup window_id=7;"));
        assert_eq!(describe(Outcome::AlreadyHuman, title), None);
    }
}
