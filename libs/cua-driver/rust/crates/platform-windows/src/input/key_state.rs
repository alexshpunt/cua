// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Modifier and button state for posted input, set only in the target's thread.
//!
//! Posted `WM_KEYDOWN(VK_CONTROL)` messages and `MK_*` flags do not change what the
//! target reads from `GetKeyState`, and most toolkits read modifiers there: a
//! posted Ctrl+A types a control character, a posted Ctrl+click is a plain click.
//! While a [`HeldKeys`] is alive, this thread is attached to the target's UI
//! thread and their shared key state reports the requested keys as down, so the
//! posted messages read as a chord. Dropping it waits until the target has
//! processed what was posted, puts the previous key state back and detaches.
//! The human's foreground thread and the global (async) key state are untouched.

use std::thread::sleep;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardState, SetKeyboardState, VK_CONTROL, VK_LBUTTON, VK_LCONTROL, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL,
};

/// Prefix of the errors when keys cannot be held for a target; nothing was posted then.
pub(crate) const KEY_STATE_UNAVAILABLE: &str = "background_key_state_unavailable";

/// Virtual keys that make a modifier read as held: the generic key and its left variant.
pub(crate) fn modifier_keys(name: &str) -> &'static [u16] {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => &[VK_CONTROL.0, VK_LCONTROL.0],
        "shift" => &[VK_SHIFT.0, VK_LSHIFT.0],
        "alt" | "menu" | "option" => &[VK_MENU.0, VK_LMENU.0],
        "win" | "meta" | "windows" | "cmd" | "command" => &[VK_LWIN.0],
        _ => &[],
    }
}

/// The virtual key of a mouse button, for toolkits that read the button from the key state.
pub(crate) fn button_key(button: &str) -> u16 {
    match button {
        "right" => VK_RBUTTON.0,
        "middle" => VK_MBUTTON.0,
        _ => VK_LBUTTON.0,
    }
}

/// `MK_CONTROL` / `MK_SHIFT` for the `wParam` of posted mouse messages.
pub(crate) fn mouse_key_flags(modifiers: &[&str]) -> usize {
    const MK_SHIFT: usize = 0x0004;
    const MK_CONTROL: usize = 0x0008;
    modifiers.iter().fold(0, |flags, name| {
        flags
            | match name.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => MK_CONTROL,
                "shift" => MK_SHIFT,
                _ => 0,
            }
    })
}

/// Keys held in the key state of one target thread; see the module docs.
pub(crate) struct HeldKeys {
    target: HWND,
    own_thread: u32,
    target_thread: u32,
    saved: [u8; 256],
    /// Posted messages the target must process before the keys go up again.
    posted: usize,
}

impl HeldKeys {
    /// Hold `keys` for messages posted to `target`. `Ok(None)` when there is nothing to
    /// hold. Fails without changing anything when the threads cannot be attached.
    pub(crate) fn hold(target: HWND, keys: &[u16]) -> anyhow::Result<Option<Self>> {
        if keys.is_empty() {
            return Ok(None);
        }
        let target_thread = unsafe { GetWindowThreadProcessId(target, None) };
        let own_thread = unsafe { GetCurrentThreadId() };
        if target_thread == 0 || target_thread == own_thread {
            anyhow::bail!(
                "{KEY_STATE_UNAVAILABLE}: the target window has no UI thread to hold keys in"
            );
        }
        if !unsafe { AttachThreadInput(own_thread, target_thread, true) }.as_bool() {
            anyhow::bail!(
                "{KEY_STATE_UNAVAILABLE}: Windows refused to attach to the target's input thread, \
                 so keys cannot be held for it in the background"
            );
        }
        let mut saved = [0u8; 256];
        let mut state = [0u8; 256];
        let read = unsafe { GetKeyboardState(&mut saved) }.is_ok();
        if read {
            state = saved;
        }
        for key in keys {
            state[*key as usize] |= 0x80;
        }
        if !read || unsafe { SetKeyboardState(&state) }.is_err() {
            unsafe {
                let _ = AttachThreadInput(own_thread, target_thread, false);
            }
            anyhow::bail!(
                "{KEY_STATE_UNAVAILABLE}: the target thread's key state could not be set"
            );
        }
        Ok(Some(Self {
            target,
            own_thread,
            target_thread,
            saved,
            posted: 0,
        }))
    }

    /// Count messages posted while the keys are held.
    pub(crate) fn posted(&mut self, count: usize) {
        self.posted += count;
    }
}

impl Drop for HeldKeys {
    fn drop(&mut self) {
        // A sent message is serviced when the target next retrieves a message, so
        // one round trip per posted message (plus one) returns after the target
        // went past everything posted under these keys.
        for _ in 0..=self.posted.min(64) {
            let mut out = 0usize;
            let _ = unsafe {
                SendMessageTimeoutW(
                    self.target,
                    WM_NULL,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    250,
                    Some(&mut out),
                )
            };
        }
        // Toolkits that coalesce input per frame (Unity, game loops) read the state
        // on their next frame.
        sleep(Duration::from_millis(30));
        unsafe {
            let _ = SetKeyboardState(&self.saved);
            let _ = AttachThreadInput(self.own_thread, self.target_thread, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_name_their_generic_and_left_keys() {
        assert_eq!(modifier_keys("Ctrl"), &[0x11, 0xA2]);
        assert_eq!(modifier_keys("shift"), &[0x10, 0xA0]);
        assert_eq!(modifier_keys("alt"), &[0x12, 0xA4]);
        assert_eq!(modifier_keys("win"), &[0x5B]);
        assert!(modifier_keys("hyper").is_empty());
    }

    #[test]
    fn mouse_flags_carry_ctrl_and_shift_only() {
        assert_eq!(mouse_key_flags(&["ctrl", "shift", "alt"]), 0x000C);
        assert_eq!(mouse_key_flags(&[]), 0);
    }

    #[test]
    fn buttons_map_to_their_virtual_keys() {
        assert_eq!(button_key("left"), 0x01);
        assert_eq!(button_key("right"), 0x02);
        assert_eq!(button_key("middle"), 0x04);
    }
}
