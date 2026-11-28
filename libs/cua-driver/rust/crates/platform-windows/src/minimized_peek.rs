// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Read the pixels of a minimized window by restoring it invisibly for the length of one capture.
//!
//! A minimized window has no fresh pixels to read. DWM stops composing it, so Windows Graphics
//! Capture delivers no frames, `PrintWindow` and `BitBlt` return black, a DWM thumbnail stays on
//! the last visible frame, and many apps (every Chromium browser) stop rendering altogether.
//! The only way to a fresh frame is to make the window non-iconic for a moment. This module does
//! that without touching what the human sees or types into:
//!
//! 1. The window gets `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` and a layered alpha
//!    of 0: fully transparent, click-through and never activated. Window animations are switched
//!    off for the window while it is restored.
//! 2. `SW_SHOWNOACTIVATE` restores it (a window minimized from maximized comes back maximized),
//!    and a `HWND_TOPMOST` z-order step keeps Chromium's occlusion tracking from hiding it again.
//! 3. After a settle time the caller captures the now ordinary window.
//! 4. The window is minimized again with `SW_SHOWMINNOACTIVE` and every changed style, z-order
//!    and animation bit is put back.
//!
//! Before the first change the original state is written to a journal; a guard undoes the change
//! on every exit path, and the next driver start undoes a change left by a driver that died.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest settle time a caller may ask for.
pub(crate) const MAX_SETTLE_MS: u64 = 2000;
/// Time the restored window gets to paint before the capture: Chromium shows its first fresh
/// frame about 12 ms after the restore and the compositor needs a few more.
pub(crate) const DEFAULT_SETTLE_MS: u64 = 150;

/// How often a capture of one flat colour is retried, and the wait before each retry.
pub(crate) const MAX_REPAINT_WAITS: u32 = 4;
pub(crate) const REPAINT_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// True when (almost) every pixel of a BGRA/RGBA frame has the colour of the first one: a window
/// that has not painted yet shows its frame colour and nothing else. Real content, even a mostly
/// empty page, has text, controls or edges.
pub(crate) fn is_flat_frame(pixels: &[u8]) -> bool {
    let Some(first) = pixels.get(..4) else {
        return false;
    };
    let total = pixels.len() / 4;
    let different = pixels
        .chunks_exact(4)
        .filter(|pixel| *pixel != first)
        .count();
    different * 1000 <= total
}

/// `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`.
pub(crate) const GHOST_BITS: u32 = 0x0008_0000 | 0x0000_0020 | 0x0800_0000;

/// What a read of a minimized window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    /// Refuse with the historical "cannot capture minimized window" error.
    Refuse,
    /// Restore the window invisibly for one capture.
    Peek { settle_ms: u64 },
}

impl Policy {
    pub(crate) fn from_args(args: &Value) -> Result<Self, String> {
        let settle_ms = match args.get("minimized_settle_ms") {
            None => DEFAULT_SETTLE_MS,
            Some(value) => value
                .as_u64()
                .filter(|ms| *ms <= MAX_SETTLE_MS)
                .ok_or_else(|| {
                    format!("minimized_settle_ms must be an integer between 0 and {MAX_SETTLE_MS}.")
                })?,
        };
        match args.get("minimized_capture") {
            None => Ok(Self::Refuse),
            Some(Value::String(mode)) if mode == "refuse" => Ok(Self::Refuse),
            Some(Value::String(mode)) if mode == "peek" => Ok(Self::Peek { settle_ms }),
            Some(_) => Err("minimized_capture must be \"refuse\" or \"peek\".".to_owned()),
        }
    }
}

/// `SetLayeredWindowAttributes` state of a window that was layered already.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Layered {
    pub key: u32,
    pub alpha: u8,
    pub flags: u32,
}

/// The window state to put back; the journal holds exactly this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Saved {
    pub hwnd: u64,
    pub pid: u32,
    pub ex_style: u32,
    pub layered: Option<Layered>,
    /// The window's own `DWMWA_TRANSITIONS_FORCEDISABLED` before the peek.
    pub transitions_disabled: bool,
}

/// The extended style of a window during the peek.
pub(crate) fn ghost_style(ex_style: u32) -> u32 {
    ex_style | GHOST_BITS
}

/// What a peek did, reported next to the pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub settle_ms: u64,
    /// How long the window stayed restored, from the restore to the minimize.
    pub held_ms: u64,
    /// The window rectangle while it was restored, in physical screen pixels.
    pub bounds: (i32, i32, i32, i32),
    /// How often the capture showed one flat colour and the driver waited for the app's first
    /// frame before capturing again.
    pub repaint_waits: u32,
    /// The app activated itself when restored and the driver handed the foreground back.
    pub foreground_returned: bool,
    /// The window that had the foreground before the peek has it again after it.
    pub foreground_kept: bool,
}

impl Report {
    pub(crate) fn to_json(self) -> Value {
        serde_json::json!({
            "mode": "peek",
            "settle_ms": self.settle_ms,
            "held_ms": self.held_ms,
            "window_bounds": {
                "x": self.bounds.0,
                "y": self.bounds.1,
                "width": self.bounds.2 - self.bounds.0,
                "height": self.bounds.3 - self.bounds.1,
            },
            "repaint_waits": self.repaint_waits,
            "foreground_returned": self.foreground_returned,
            "foreground_kept": self.foreground_kept,
        })
    }
}

#[cfg(target_os = "windows")]
pub(crate) use native::{png_is_flat, recover_stale, run};

#[cfg(target_os = "windows")]
mod native {
    use super::{ghost_style, Layered, Report, Saved, GHOST_BITS, MAX_REPAINT_WAITS, REPAINT_WAIT};
    use anyhow::{bail, Context, Result};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetLayeredWindowAttributes, GetWindowLongW, GetWindowRect,
        GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible,
        SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, ShowWindow, GWL_EXSTYLE,
        HWND_NOTOPMOST, HWND_TOPMOST, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, WS_EX_LAYERED,
        WS_EX_TOPMOST,
    };

    /// One ghost at a time: a second concurrent peek would only add risk.
    static ONE_PEEK: Mutex<()> = Mutex::new(());
    const RESTORE_TIMEOUT: Duration = Duration::from_millis(1500);

    /// Whether a PNG shows one flat colour: the first frame of an app has not arrived yet.
    pub(crate) fn png_is_flat(png: &[u8]) -> bool {
        // A flat image compresses to a few kilobytes; skip decoding anything larger.
        if png.len() > 64 * 1024 {
            return false;
        }
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map(|decoded| super::is_flat_frame(decoded.to_rgba8().as_raw()))
            .unwrap_or(false)
    }

    fn hwnd_of(raw: u64) -> HWND {
        HWND(raw as *mut _)
    }

    const JOURNAL_PREFIX: &str = "minimized-peek-journal-";

    fn journal_dir() -> PathBuf {
        std::env::var_os("CUA_DRIVER_RS_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("cua-driver"))
            })
            .unwrap_or_else(std::env::temp_dir)
    }

    /// One journal per driver process, so a driver never undoes a peek another live driver runs.
    fn journal_path() -> PathBuf {
        journal_dir().join(format!("{JOURNAL_PREFIX}{}.json", std::process::id()))
    }

    fn write_journal(saved: &Saved) -> Result<()> {
        let path = journal_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let staging = path.with_extension("json.tmp");
        std::fs::write(&staging, serde_json::to_vec(saved)?)?;
        std::fs::rename(&staging, &path)?;
        Ok(())
    }

    fn remove_journal() {
        let _ = std::fs::remove_file(journal_path());
    }

    fn process_is_alive(pid: u32) -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut code = 0u32;
            let alive = GetExitCodeProcess(process, &mut code).is_ok() && code == STILL_ACTIVE;
            let _ = CloseHandle(process);
            alive
        }
    }

    unsafe fn ex_style(hwnd: HWND) -> u32 {
        GetWindowLongW(hwnd, GWL_EXSTYLE) as u32
    }

    unsafe fn read_transitions_disabled(hwnd: HWND) -> bool {
        let mut value: i32 = 0;
        let read = DwmGetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &mut value as *mut _ as *mut _,
            4,
        );
        read.is_ok() && value != 0
    }

    unsafe fn set_transitions_disabled(hwnd: HWND, disabled: bool) {
        let value: i32 = i32::from(disabled);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &value as *const _ as *const _,
            4,
        );
    }

    /// Put the window back as it was. Safe to call again and on a window that is gone.
    unsafe fn undo(saved: &Saved, minimize: bool, topmost_set: bool) {
        let hwnd = hwnd_of(saved.hwnd);
        if !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        // A reused handle belongs to someone else: never touch it.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != saved.pid {
            return;
        }
        // Minimize first, while the window is still invisible.
        if minimize {
            let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
        }
        if topmost_set {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        set_transitions_disabled(hwnd, saved.transitions_disabled);
        match saved.layered {
            Some(layered) => {
                let _ = SetLayeredWindowAttributes(
                    hwnd,
                    COLORREF(layered.key),
                    layered.alpha,
                    LAYERED_WINDOW_ATTRIBUTES_FLAGS(layered.flags),
                );
            }
            None => {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
        }
        SetWindowLongW(hwnd, GWL_EXSTYLE, saved.ex_style as i32);
    }

    /// Give the foreground back to the window that had it when the app took it for itself.
    /// Returns whether the foreground had to be returned.
    unsafe fn give_foreground_back(window: HWND, before: HWND) -> bool {
        if before == window || GetForegroundWindow() != window {
            return false;
        }
        restore_foreground(before);
        true
    }

    /// Make `before` the foreground window again if it is still an ordinary visible window.
    unsafe fn restore_foreground(before: HWND) -> bool {
        if before.0.is_null()
            || !IsWindow(Some(before)).as_bool()
            || IsIconic(before).as_bool()
            || !IsWindowVisible(before).as_bool()
            || GetForegroundWindow() == before
        {
            return false;
        }
        // The app that took the foreground is not this process: Windows may refuse a plain request.
        crate::input::inject::force_foreground_assisted(before).0
    }

    /// Undoes the peek on every exit path, including a panic in the capture.
    struct Ghost {
        saved: Saved,
        restored: bool,
        topmost_set: bool,
        done: bool,
    }

    impl Ghost {
        fn finish(&mut self, minimize: bool) {
            if self.done {
                return;
            }
            self.done = true;
            unsafe { undo(&self.saved, minimize && self.restored, self.topmost_set) };
            remove_journal();
        }
    }

    impl Drop for Ghost {
        fn drop(&mut self) {
            self.finish(true);
        }
    }

    /// Restore the windows of drivers that died during a peek. Called at start-up.
    pub(crate) fn recover_stale() {
        let Ok(entries) = std::fs::read_dir(journal_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(owner) = name
                .strip_prefix(JOURNAL_PREFIX)
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|pid| pid.parse::<u32>().ok())
            else {
                continue;
            };
            if owner == std::process::id() || process_is_alive(owner) {
                continue;
            }
            let path = entry.path();
            match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Saved>(&bytes).map_err(|error| error.to_string())
                }) {
                Ok(saved) => unsafe {
                    let foreground = GetForegroundWindow() == hwnd_of(saved.hwnd);
                    undo(&saved, !foreground, true);
                    tracing::warn!(
                        target: "cua-driver",
                        "minimized peek: undid a change left by driver {owner} on window 0x{:x}",
                        saved.hwnd
                    );
                },
                Err(error) => tracing::warn!(
                    target: "cua-driver",
                    "minimized peek: ignoring an unreadable journal: {error}"
                ),
            }
            let _ = std::fs::remove_file(path);
        }
    }

    /// Restore the minimized window invisibly, wait `settle_ms`, run `capture` on the ordinary
    /// window, then minimize it again and undo every change. A refusal says why the window was
    /// left untouched.
    pub(crate) fn run<T>(
        hwnd: u64,
        settle_ms: u64,
        mut capture: impl FnMut() -> Result<T>,
        unpainted: impl Fn(&T) -> bool,
    ) -> Result<(T, Report)> {
        let _one = ONE_PEEK.lock().unwrap_or_else(|poison| poison.into_inner());
        let window = hwnd_of(hwnd);
        unsafe {
            if !IsWindow(Some(window)).as_bool() {
                bail!("cannot capture minimized window 0x{hwnd:x}: it no longer exists");
            }
            if !IsIconic(window).as_bool() {
                bail!("cannot peek at window 0x{hwnd:x}: it is not minimized");
            }
            if IsHungAppWindow(window).as_bool() {
                bail!(
                    "cannot capture minimized window 0x{hwnd:x}: its app is not responding, \
                     so it cannot be restored"
                );
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            let original = ex_style(window);
            let layered = if original & WS_EX_LAYERED.0 != 0 {
                let mut key = COLORREF(0);
                let mut alpha = 255u8;
                let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
                GetLayeredWindowAttributes(
                    window,
                    Some(&mut key),
                    Some(&mut alpha),
                    Some(&mut flags),
                )
                .map_err(|_| {
                    anyhow::anyhow!(
                        "cannot capture minimized window 0x{hwnd:x}: it draws with per-pixel \
                         alpha, which cannot be made invisible and restored exactly"
                    )
                })?;
                Some(Layered {
                    key: key.0,
                    alpha,
                    flags: flags.0,
                })
            } else {
                None
            };
            let saved = Saved {
                hwnd,
                pid,
                ex_style: original,
                layered,
                transitions_disabled: read_transitions_disabled(window),
            };
            write_journal(&saved).context("cannot record the window state before changing it")?;
            let mut ghost = Ghost {
                saved,
                restored: false,
                topmost_set: false,
                done: false,
            };

            // 1. Invisible, click-through, never activated.
            SetWindowLongW(window, GWL_EXSTYLE, ghost_style(original) as i32);
            let applied = ex_style(window) & GHOST_BITS == GHOST_BITS;
            if !applied || SetLayeredWindowAttributes(window, COLORREF(0), 0, LWA_ALPHA).is_err() {
                ghost.finish(false);
                bail!(
                    "cannot capture minimized window 0x{hwnd:x}: Windows refused to change its \
                     style (an elevated app or another desktop)"
                );
            }
            set_transitions_disabled(window, true);

            // 2. Restore without activation; stay on top so occlusion tracking keeps it visible.
            let foreground_before = GetForegroundWindow();
            let restored_at = Instant::now();
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            ghost.restored = true;
            while IsIconic(window).as_bool() {
                if restored_at.elapsed() > RESTORE_TIMEOUT {
                    ghost.finish(true);
                    bail!(
                        "cannot capture minimized window 0x{hwnd:x}: it did not restore \
                         within {} ms",
                        RESTORE_TIMEOUT.as_millis()
                    );
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if original & WS_EX_TOPMOST.0 == 0
                && SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
                .is_ok()
            {
                ghost.topmost_set = true;
            }

            // 3. Let the app paint. Some apps activate themselves when restored: the foreground goes
            // back at once, so the human's keystrokes do not land in a transparent window.
            let mut foreground_returned = false;
            let settle = Duration::from_millis(settle_ms);
            let settle_started = Instant::now();
            while settle_started.elapsed() < settle {
                std::thread::sleep(Duration::from_millis(10).min(settle));
                foreground_returned |= give_foreground_back(window, foreground_before);
            }
            foreground_returned |= give_foreground_back(window, foreground_before);
            let mut rect = RECT::default();
            let _ = GetWindowRect(window, &mut rect);
            let mut outcome = capture();
            // An app that was hidden for a while can need longer than the settle time for its
            // first frame; until then the capture shows its frame colour and nothing else.
            let mut repaint_waits = 0u32;
            while repaint_waits < MAX_REPAINT_WAITS
                && outcome.as_ref().map(&unpainted).unwrap_or(false)
            {
                repaint_waits += 1;
                std::thread::sleep(REPAINT_WAIT);
                foreground_returned |= give_foreground_back(window, foreground_before);
                let _ = GetWindowRect(window, &mut rect);
                outcome = capture();
            }
            let held_ms = restored_at.elapsed().as_millis() as u64;
            foreground_returned |= give_foreground_back(window, foreground_before);
            ghost.finish(true);
            // Minimizing an active window activates the next one in z-order, not the human's, and
            // an app that took the foreground once may take it again as its restore settles.
            if foreground_returned {
                for _ in 0..4 {
                    restore_foreground(foreground_before);
                    std::thread::sleep(Duration::from_millis(40));
                }
                restore_foreground(foreground_before);
            }
            foreground_returned |= give_foreground_back(window, foreground_before);
            tracing::debug!(
                target: "cua-driver",
                "minimized peek of 0x{hwnd:x}: foreground returned {foreground_returned}, now {:?}, before {:?}",
                GetForegroundWindow(),
                foreground_before
            );
            let report = Report {
                settle_ms,
                held_ms,
                bounds: (rect.left, rect.top, rect.right, rect.bottom),
                repaint_waits,
                foreground_returned,
                foreground_kept: foreground_before.0.is_null()
                    || GetForegroundWindow() == foreground_before,
            };
            outcome.map(|captured| (captured, report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reading_a_minimized_window_refuses_unless_the_caller_asks_to_peek() {
        assert_eq!(Policy::from_args(&json!({})), Ok(Policy::Refuse));
        assert_eq!(
            Policy::from_args(&json!({"minimized_capture": "refuse"})),
            Ok(Policy::Refuse)
        );
        assert_eq!(
            Policy::from_args(&json!({"minimized_capture": "peek"})),
            Ok(Policy::Peek {
                settle_ms: DEFAULT_SETTLE_MS
            })
        );
    }

    #[test]
    fn the_settle_time_is_bounded_and_only_read_for_a_peek() {
        assert_eq!(
            Policy::from_args(&json!({"minimized_capture": "peek", "minimized_settle_ms": 0})),
            Ok(Policy::Peek { settle_ms: 0 })
        );
        assert_eq!(
            Policy::from_args(
                &json!({"minimized_capture": "peek", "minimized_settle_ms": MAX_SETTLE_MS})
            ),
            Ok(Policy::Peek {
                settle_ms: MAX_SETTLE_MS
            })
        );
        for bad in [json!(2001), json!(-1), json!("150"), json!(1.5)] {
            assert!(Policy::from_args(
                &json!({"minimized_capture": "peek", "minimized_settle_ms": bad})
            )
            .is_err());
        }
    }

    #[test]
    fn unknown_modes_are_refused_not_guessed() {
        for bad in [json!("restore"), json!(true), json!(null), json!("PEEK")] {
            assert_eq!(
                Policy::from_args(&json!({"minimized_capture": bad})),
                Err("minimized_capture must be \"refuse\" or \"peek\".".to_owned())
            );
        }
    }

    #[test]
    fn the_ghost_style_adds_exactly_the_three_bits_and_keeps_the_rest() {
        let topmost_tool_window = 0x0000_0108u32;
        let ghost = ghost_style(topmost_tool_window);
        assert_eq!(ghost & GHOST_BITS, GHOST_BITS);
        assert_eq!(ghost & !GHOST_BITS, topmost_tool_window);
        assert_eq!(ghost_style(ghost), ghost);
    }

    #[test]
    fn the_journal_round_trips_every_field_needed_to_undo() {
        let saved = Saved {
            hwnd: 0x2ee154a,
            pid: 20228,
            ex_style: 0x0020_0100,
            layered: Some(Layered {
                key: 0x00ff_00ff,
                alpha: 200,
                flags: 2,
            }),
            transitions_disabled: true,
        };
        let bytes = serde_json::to_vec(&saved).unwrap();
        assert_eq!(serde_json::from_slice::<Saved>(&bytes).unwrap(), saved);
    }

    #[test]
    fn a_frame_of_one_colour_is_unpainted_but_any_content_is_not() {
        let flat = [214u8, 226, 251, 255].repeat(10_000);
        assert!(is_flat_frame(&flat));
        // A dark page with a few pixels of text is real content.
        let mut page = [16u8, 26, 50, 255].repeat(10_000);
        for pixel in page.chunks_exact_mut(4).take(200) {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
        assert!(!is_flat_frame(&page));
        assert!(!is_flat_frame(&[]));
        assert!(!is_flat_frame(&[1, 2, 3]));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_flat_png_is_detected_and_a_painted_one_is_not() {
        let encode = |pixels: &[u8]| {
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(pixels, 100, 80, image::ExtendedColorType::Rgba8)
                .unwrap();
            png
        };
        use image::ImageEncoder;
        let flat = encode(&[214u8, 226, 251, 255].repeat(100 * 80));
        assert!(png_is_flat(&flat));
        let mut painted = [16u8, 26, 50, 255].repeat(100 * 80);
        for (i, pixel) in painted.chunks_exact_mut(4).enumerate() {
            if i % 7 == 0 {
                pixel.copy_from_slice(&[(i % 251) as u8, 200, 90, 255]);
            }
        }
        assert!(!png_is_flat(&encode(&painted)));
        assert!(!png_is_flat(b"not a png"));
    }

    #[test]
    fn the_report_names_the_restored_bounds_and_the_hold_time() {
        let report = Report {
            settle_ms: 150,
            held_ms: 480,
            bounds: (300, 180, 1650, 1230),
            repaint_waits: 0,
            foreground_returned: false,
            foreground_kept: true,
        };
        assert_eq!(
            report.to_json(),
            json!({
                "mode": "peek",
                "settle_ms": 150,
                "held_ms": 480,
                "window_bounds": {"x": 300, "y": 180, "width": 1350, "height": 1050},
                "repaint_waits": 0,
                "foreground_returned": false,
                "foreground_kept": true,
            })
        );
    }
}
