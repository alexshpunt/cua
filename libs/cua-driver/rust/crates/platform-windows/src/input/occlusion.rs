//! Which window a real pointer press at a screen point reaches.
//!
//! `SendInput` goes to whatever window is on top at the point. A press planned
//! on one window's image lands in a dialog or another app's window that covers
//! it, and nothing in the receipt tells. Every foreground press checks its point
//! first and refuses before pressing when another window covers it.

use std::fmt;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsWindowVisible,
    WindowFromPoint, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_HWNDNEXT, GW_OWNER, WS_CAPTION,
    WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// How a covering window relates to the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
    /// Owned by the target, such as its dialog. It stays in front of the target
    /// even when the target is activated.
    OwnedWindow,
    /// Another top-level window of the target's process.
    SameProcess,
    /// A window of another process.
    OtherProcess,
}

impl Relation {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Relation::OwnedWindow => "owned_window",
            Relation::SameProcess => "same_process",
            Relation::OtherProcess => "other_process",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Relation::OwnedWindow => "a window the target owns",
            Relation::SameProcess => "another window of the same app",
            Relation::OtherProcess => "a window of another app",
        }
    }
}

/// Facts about the top-level window under the point.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Hit {
    pub is_target: bool,
    pub owned_by_target: bool,
    pub same_process: bool,
    pub captioned: bool,
}

/// `None` when a press at the point reaches the target. The app's own popups,
/// such as menus, drop-downs and tooltips, have no caption and belong to it.
pub(crate) fn covering(hit: Hit) -> Option<Relation> {
    if hit.is_target || (hit.same_process && !hit.captioned) {
        return None;
    }
    Some(if hit.owned_by_target {
        Relation::OwnedWindow
    } else if hit.same_process {
        Relation::SameProcess
    } else {
        Relation::OtherProcess
    })
}

/// A refused press: the window that would have received it instead.
#[derive(Clone, Debug)]
pub struct Occluded {
    pub point: (i32, i32),
    pub window_id: u64,
    pub pid: u32,
    pub title: String,
    pub relation: Relation,
}

pub(crate) const CODE: &str = "pointer_target_occluded";

impl fmt::Display for Occluded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{CODE}: screen point ({},{}) is covered by {}: window_id={} pid={} relation={} title={}; nothing was pressed there",
            self.point.0,
            self.point.1,
            self.relation.describe(),
            self.window_id,
            self.pid,
            self.relation.name(),
            serde_json::to_string(&self.title).unwrap_or_else(|_| "\"\"".into()),
        )
    }
}

impl std::error::Error for Occluded {}

fn hwnd(value: u64) -> HWND {
    HWND(value as usize as *mut _)
}

fn root_of(window: HWND) -> HWND {
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    if root.0.is_null() {
        window
    } else {
        root
    }
}

/// Windows that never take the press: click-through layered windows and the
/// driver's own windows, such as the agent cursor.
fn passthrough(window: HWND) -> bool {
    let ex = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) } as u32;
    let click_through = ex & WS_EX_LAYERED.0 != 0 && ex & WS_EX_TRANSPARENT.0 != 0;
    click_through
        || crate::win32::window_owner_pid(window.0 as usize as u64)
            == Some(unsafe { GetCurrentProcessId() })
}

fn contains(window: HWND, x: i32, y: i32) -> bool {
    let mut rect = RECT::default();
    unsafe { IsWindowVisible(window) }.as_bool()
        && unsafe { GetWindowRect(window, &mut rect) }.is_ok()
        && x >= rect.left
        && x < rect.right
        && y >= rect.top
        && y < rect.bottom
}

/// The top-level window a press at the screen point reaches.
fn top_window_at(x: i32, y: i32) -> Option<HWND> {
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    if hit.0.is_null() {
        return None;
    }
    let mut current = root_of(hit);
    for _ in 0..512 {
        if !passthrough(current) {
            return Some(current);
        }
        // Below a pass-through window, the next visible top-level window that
        // contains the point takes the press.
        loop {
            current = unsafe { GetWindow(current, GW_HWNDNEXT) }.ok()?;
            if current.0.is_null() {
                return None;
            }
            if contains(current, x, y) {
                break;
            }
        }
    }
    None
}

fn owner_chain_reaches(target: HWND, window: HWND) -> bool {
    let mut current = window;
    for _ in 0..64 {
        let owner = unsafe { GetWindow(current, GW_OWNER) }.unwrap_or_default();
        if owner.0.is_null() || owner == current {
            return false;
        }
        if owner == target {
            return true;
        }
        current = owner;
    }
    false
}

fn title(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetWindowTextW(window, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length.min(buffer.len())])
}

/// Refuses when a press at screen point `(x, y)` would reach another window
/// than `target`. Call it after the target is activated or raised, right before
/// the press. When Windows reports no window at the point, the press goes on.
pub(crate) fn check_point(target: u64, x: i32, y: i32) -> Result<(), Occluded> {
    let target = root_of(hwnd(target));
    let Some(top) = top_window_at(x, y) else {
        return Ok(());
    };
    let target_pid = crate::win32::window_owner_pid(target.0 as usize as u64);
    let pid = crate::win32::window_owner_pid(top.0 as usize as u64);
    let style = unsafe { GetWindowLongPtrW(top, GWL_STYLE) } as u32;
    let hit = Hit {
        is_target: top == target,
        owned_by_target: owner_chain_reaches(target, top),
        same_process: pid.is_some() && pid == target_pid,
        captioned: style & WS_CAPTION.0 == WS_CAPTION.0,
    };
    match covering(hit) {
        None => Ok(()),
        Some(relation) => Err(Occluded {
            point: (x, y),
            window_id: top.0 as usize as u64,
            pid: pid.unwrap_or(0),
            title: title(top),
            relation,
        }),
    }
}

/// Tool result for a failed foreground press: an occluded point is a refusal,
/// since nothing was pressed; any other error keeps its message.
pub(crate) fn input_error(error: anyhow::Error) -> cua_driver_core::protocol::ToolResult {
    use cua_driver_core::protocol::ToolResult;
    match error.downcast_ref::<Occluded>() {
        Some(occluded) => ToolResult::error(occluded.to_string()).with_structured(
            serde_json::json!({"status": "refused", "code": CODE, "effect": "refused"}),
        ),
        None => ToolResult::error(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_and_its_popups_take_the_press() {
        assert_eq!(
            covering(Hit {
                is_target: true,
                ..Hit::default()
            }),
            None
        );
        let menu = Hit {
            same_process: true,
            owned_by_target: true,
            ..Hit::default()
        };
        assert_eq!(covering(menu), None);
        let unowned_popup = Hit {
            same_process: true,
            ..Hit::default()
        };
        assert_eq!(covering(unowned_popup), None);
    }

    #[test]
    fn a_dialog_of_the_target_covers_it() {
        let dialog = Hit {
            same_process: true,
            owned_by_target: true,
            captioned: true,
            ..Hit::default()
        };
        assert_eq!(covering(dialog), Some(Relation::OwnedWindow));
    }

    #[test]
    fn another_window_of_the_app_covers_it() {
        let sibling = Hit {
            same_process: true,
            captioned: true,
            ..Hit::default()
        };
        assert_eq!(covering(sibling), Some(Relation::SameProcess));
    }

    #[test]
    fn any_window_of_another_app_covers_it() {
        assert_eq!(covering(Hit::default()), Some(Relation::OtherProcess));
        assert_eq!(
            covering(Hit {
                captioned: true,
                ..Hit::default()
            }),
            Some(Relation::OtherProcess)
        );
    }

    #[test]
    fn the_refusal_names_the_covering_window() {
        let occluded = Occluded {
            point: (10, -4),
            window_id: 0x2a,
            pid: 7,
            title: "Change \"Foreground\" Color".into(),
            relation: Relation::OwnedWindow,
        };
        assert_eq!(
            occluded.to_string(),
            "pointer_target_occluded: screen point (10,-4) is covered by a window the target owns: \
             window_id=42 pid=7 relation=owned_window title=\"Change \\\"Foreground\\\" Color\"; \
             nothing was pressed there"
        );
    }

    #[test]
    fn an_occluded_press_is_a_refusal() {
        let error = anyhow::Error::new(Occluded {
            point: (1, 2),
            window_id: 3,
            pid: 4,
            title: String::new(),
            relation: Relation::OtherProcess,
        });
        let structured = input_error(error).structured_content.unwrap();
        assert_eq!(structured["code"], CODE);
        assert_eq!(structured["effect"], "refused");
        let other = input_error(anyhow::anyhow!("foreground_unavailable: x"));
        assert!(other.structured_content.is_none());
    }
}
