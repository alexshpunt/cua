// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Real Windows tool implementations (compiled only on Windows).

use async_trait::async_trait;

mod activity;
mod pointer;
mod scroll;
mod window_management;
pub(crate) mod window_restore;

/// Pin the agent-cursor overlay above `hwnd` in the z-order, normalising to
/// the **root** ancestor so the pin lands on the window that actually appears
/// in the global z-stack.
///
/// For UWP / packaged apps the inner `Windows.UI.Core.CoreWindow` is hosted
/// inside `ApplicationFrameHost.exe` — UIA targets the inner window but only
/// the host appears in the z-order. `GA_ROOT` normalises both inputs to the
/// host so the overlay sits at z+1 of whatever is actually painted on screen.
///
/// No-op when the overlay is disabled or `key` is empty (a direct platform
/// call without lifecycle metadata); the render thread drops the command.
pub(crate) fn pin_overlay_above(key: &str, hwnd: u64) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOT};
    let root = unsafe { GetAncestor(HWND(hwnd as *mut _), GA_ROOT) };
    let wid = if !root.0.is_null() {
        root.0 as u64
    } else {
        hwnd
    };
    crate::overlay::send_command(
        key.to_owned(),
        cursor_overlay::OverlayCommand::PinAbove(wid),
    );
}

/// Place and reveal a named session's cursor before keyboard or text input.
///
/// `explicit` is the targeted element's screen centre when the action has
/// one. Without it the cursor reuses its remembered position, then falls back
/// to the target window's centre, so the first keyboard action of a session
/// is visible, and last to the hardware pointer. Anonymous calls keep their
/// existing behavior. Geometry and overlay failures are observational and
/// never affect the input result.
pub(crate) async fn position_keyboard_cursor(
    state: &ToolState,
    args: &Value,
    hwnd: u64,
    explicit: Option<(f64, f64)>,
) {
    if cursor_overlay::named_session_cursor_key(args).is_none() {
        return;
    }
    let cursor_key = resolve_cursor_key(args);
    let remembered = state
        .cursor_registry
        .get(&cursor_key)
        .and_then(|cursor| cursor.x.zip(cursor.y));
    let (window_center, pointer) =
        if cursor_overlay::keyboard_cursor_target(explicit, remembered, None, None).is_none() {
            let center = (hwnd != 0).then(|| keyboard_window_center(hwnd)).flatten();
            (center, center.is_none().then(pointer_position).flatten())
        } else {
            (None, None)
        };
    let Some((x, y)) =
        cursor_overlay::keyboard_cursor_target(explicit, remembered, window_center, pointer)
    else {
        return;
    };
    if hwnd != 0 {
        pin_overlay_above(&cursor_key, hwnd);
    }
    overlay_glide_to(&cursor_key, x, y).await;
    state.cursor_registry.update_position(&cursor_key, x, y);
}

fn pointer_position() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((f64::from(point.x), f64::from(point.y)))
}

fn keyboard_window_center(hwnd: u64) -> Option<(f64, f64)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut rect = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut rect) }.ok()?;
    (rect.right > rect.left && rect.bottom > rect.top).then(|| {
        (
            f64::from(rect.left + rect.right) / 2.0,
            f64::from(rect.top + rect.bottom) / 2.0,
        )
    })
}

/// Resolve the screen point a coordinate action should actuate at, scrolling
/// the target element into view first when its cached center lands off-screen.
///
/// The element cache captures each element's center at walk time. On a short
/// display a tall window reflows so some controls sit below the visible area;
/// their cached center then falls outside the window rect and a raw tap would
/// land on the taskbar / another window instead of the control. Before failing,
/// we ask the control to scroll itself visible via UIA
/// `ScrollItemPattern::ScrollIntoView` (the control drives its own
/// ScrollViewer), then re-read its *live* bounding-rect center.
///
/// Returns:
/// - `Ok((cx,cy))` unchanged when already in bounds — fast path, no COM call;
/// - `Ok((nx,ny))` with the post-scroll center when scrolling brought it in;
/// - `Err(result)` with a typed refusal when minimized or still off-screen — a
///   clear error beats a misfire onto the taskbar.
///
/// Background-safe: ScrollIntoView is delivered over the accessibility channel
/// (wrapped in the UWP fg-steal bypass) and does not raise or activate the
/// window. MSAA-walked elements are skipped (no ScrollItemPattern) and keep the
/// existing failure.
pub(crate) fn resolve_onscreen_point_with_scroll(
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    hwnd: u64,
    idx: usize,
    cx: i32,
    cy: i32,
    action_verb: &str,
) -> Result<(i32, i32), ToolResult> {
    // Issue #2015: refuse the iconic sentinel BEFORE the containment check
    // below, which structurally cannot catch it. A minimized window's
    // `GetWindowRect` is the off-screen iconic position
    // `(-32000, -32000)-(-31840, -31972)`, and UIA mirrors that same sentinel
    // into every element's `BoundingRectangle`. Both sides of
    // `point_in_window_bounds` are then poisoned identically, so a sentinel
    // center tests as *inside* its sentinel window rect, the guard passes, and
    // the click is posted to coordinates no monitor covers — the tool reports
    // success and nothing happens. `capture.rs` already refuses iconic windows
    // outright; the input path must refuse them too rather than silently no-op.
    if let Some(refusal) = minimized_element_refusal(hwnd, idx, cx, cy, action_verb) {
        return Err(refusal);
    }
    let cached_center_in_window = crate::input::point_in_window_bounds(hwnd, cx, cy);
    if cached_center_in_window && !crate::input::point_on_virtual_desktop(cx, cy) {
        let message = format!(
            "Element [{idx}] resolves to ({cx},{cy}) inside window 0x{hwnd:x}, but \
             that point is outside the live Windows virtual desktop. The bounds \
             may be a stale iconic position, so {action_verb} was refused before \
             input dispatch. Restore or move the window onto a display, then \
             re-snapshot with get_window_state before retrying."
        );
        return Err(ToolResult::error(message.clone()).with_structured(json!({
            "status": "refused",
            "code": "element_not_visible",
            "effect": "refused",
            "refusal": {
                "code": "element_not_visible",
                "message": message,
            }
        })));
    }
    // A center can still be clipped by a ScrollViewer or a docked sibling while
    // remaining inside the outer HWND rectangle. UIA's IsOffscreen property is
    // authoritative for that case; without it a foreground tap can land on the
    // visible control covering the stale point (for example a bottom toolbar).
    if let Some(retained) = admitted.as_ref() {
        // Geometry read while minimized is stale even when it happens to land
        // inside the restored window rect.
        let trust_cached = cached_center_in_window && !retained.read_while_minimized;
        if retained.is_uia() {
            let is_offscreen =
                unsafe { crate::uia::scroll::element_is_offscreen(retained.as_ptr()) };
            if trust_cached && is_offscreen != Some(true) {
                return Ok((cx, cy));
            }
            if let Some((nx, ny)) = unsafe {
                crate::uia::scroll::scroll_into_view_and_recenter(hwnd, retained.as_ptr())
            } {
                if crate::input::point_in_window_bounds(hwnd, nx, ny) {
                    return Ok((nx, ny));
                }
            }
        } else if trust_cached {
            return Ok((cx, cy));
        }
        // `retained` drops here → COM Release.
    } else if cached_center_in_window {
        return Ok((cx, cy));
    }
    let message = format!(
        "Element [{idx}] resolves to ({cx},{cy}) but is not visibly actionable — \
         tried UIA ScrollIntoView but it is still off-screen, so {action_verb} \
         would land on whatever covers that point. Make the window taller or \
         scroll the region into view, then retry."
    );
    Err(ToolResult::error(message.clone()).with_structured(json!({
        "status": "refused",
        "code": "element_not_visible",
        "effect": "refused",
        "refusal": {
            "code": "element_not_visible",
            "message": message,
        }
    })))
}

/// Re-read `IsEnabled` on an admitted token target and refuse a disabled one
/// before any cursor or input work, as macOS does (`element_disabled`). The
/// snapshot's `enabled` can be stale. An element whose state cannot be read
/// is left to the tool.
pub(crate) async fn refuse_if_disabled(
    pid: u32,
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    tool: &str,
) -> Option<ToolResult> {
    let element = admitted.clone()?;
    let enabled = tokio::task::spawn_blocking(move || element.element_is_enabled())
        .await
        .ok()?;
    (enabled == Some(false)).then(|| element_disabled_refusal(pid, tool))
}

pub(crate) fn element_disabled_refusal(pid: u32, tool: &str) -> ToolResult {
    ToolResult::error(format!(
        "refusing {tool}: the target reports IsEnabled=false, so the app would ignore it. \
         Nothing was sent. Snapshot again and act once the element reports enabled"
    ))
    .with_structured(json!({
        "code": "element_disabled",
        "effect": "refused",
        "pid": pid,
        "action": tool,
    }))
}

pub(crate) fn minimized_element_refusal(
    hwnd: u64,
    idx: usize,
    cx: i32,
    cy: i32,
    action_verb: &str,
) -> Option<ToolResult> {
    crate::input::window_is_iconic(hwnd).then(|| {
        let message = format!(
            "Element [{idx}] resolves to ({cx},{cy}), which is the Win32 iconic \
             sentinel rather than a location on any display — window 0x{hwnd:x} \
             is minimized, so {action_verb} would \
             be posted off-screen and silently do nothing. Call bring_to_front \
             with this window_id to restore it, then re-snapshot with \
             get_window_state before retrying."
        );
        ToolResult::error(format!("refused (window_minimized): {message}")).with_structured(json!({
            "status": "refused",
            "code": "window_minimized",
            "effect": "refused",
            "refusal": {
                "code": "window_minimized",
                "message": message,
            }
        }))
    })
}

/// Convert (px, py) — "window-local screenshot pixels, top-left origin
/// of the PNG returned by `get_window_state`" — to screen coordinates.
///
/// The capture path (`crate::capture::screenshot_window`) takes the
/// PrintWindow buffer (sized to `GetWindowRect`) and crops it to
/// `DWMWA_EXTENDED_FRAME_BOUNDS` with a 1-pixel inset on each side
/// (`DWM_CROP_INSET_PX`). So the bitmap's top-left in screen coords is
/// `(dwm_frame.left + 1, dwm_frame.top + 1)` — and CALLER coordinates
/// `(px, py)` are offsets relative to that origin.
///
/// Pre-PR-#1697 the click / right_click / double_click / drag impls
/// called `ClientToScreen(px, py)`, which interprets the offsets as
/// CLIENT-area relative — so the screen click landed ~30 px BELOW the
/// bitmap location the agent intended (the title-bar height got added).
/// This helper aligns the screen mapping with what the screenshot
/// actually shows.
///
/// Fallbacks: if the DWM call fails (very old Windows / shell-extension
/// hook) we degrade to `GetWindowRect.top-left + (px, py)` — matches
/// the capture fallback which also keeps the full PrintWindow bitmap
/// without crop.
pub(crate) fn bitmap_to_screen(hwnd: u64, px: i32, py: i32) -> (i32, i32) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    // Keep this constant in sync with `capture::DWM_CROP_INSET_PX`.
    const DWM_CROP_INSET_PX: i32 = 1;
    let h = HWND(hwnd as *mut _);
    unsafe {
        let mut dwm = RECT::default();
        let hr = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut dwm as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if hr.is_ok() {
            return (
                dwm.left + DWM_CROP_INSET_PX + px,
                dwm.top + DWM_CROP_INSET_PX + py,
            );
        }
        // Fallback path — capture also keeps the full bitmap when DWM
        // bounds aren't available, so the bitmap origin IS GetWindowRect
        // top-left in that branch.
        let mut wr = RECT::default();
        let _ = GetWindowRect(h, &mut wr);
        (wr.left + px, wr.top + py)
    }
}

/// Queue movement in screen coordinates on the presentation timeline. Never
/// await the render thread or sleep before real input. An empty lifecycle key
/// or a disabled overlay leaves input unchanged.
pub(crate) async fn overlay_glide_to(key: &str, sx: f64, sy: f64) {
    overlay_glide_to_target(key, sx, sy, None).await;
}

/// [`overlay_glide_to`] with the targeted element's screen rect
/// `[x, y, width, height]` (same space as `sx`/`sy`); `None` for pixel actions.
pub(crate) async fn overlay_glide_to_target(key: &str, sx: f64, sy: f64, target: Option<[f64; 4]>) {
    if key.is_empty() {
        return;
    }
    if !crate::overlay::is_enabled(key) {
        return;
    }
    // Presentation has its own bounded render-side playback. Input must never
    // sleep for a visual glide or depend on render-thread responsiveness.
    crate::overlay::send_command(
        key.to_owned(),
        cursor_overlay::OverlayCommand::MoveTo {
            x: sx,
            y: sy,
            end_heading_radians: std::f64::consts::FRAC_PI_4,
            target,
        },
    );
}

/// The admitted element's cached UIA/MSAA bounding rect as the overlay's
/// `[x, y, width, height]`, in the same screen space as its cached `center`
/// (and so as the glide point). `(dx, dy)` shifts it when the glide point
/// moved off the cached center (scroll-into-view re-resolution).
pub(crate) fn element_target_rect(
    element: Option<&crate::uia::snapshot::RetainedElement>,
    (dx, dy): (i32, i32),
) -> Option<[f64; 4]> {
    let (l, t, r, b) = element?.rect?;
    (r > l && b > t).then(|| {
        [
            (l + dx) as f64,
            (t + dy) as f64,
            (r - l) as f64,
            (b - t) as f64,
        ]
    })
}

use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef, ToolRegistry},
    window_target::{PidOnlyWindowTargetGuard, WindowTargetCandidate, WindowTargetCandidates},
};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};

use crate::uia::Snapshots;
use cursor_overlay::CursorRegistry;

use super::{
    bring_to_front::BringToFrontTool,
    check_permissions::CheckPermissionsTool,
    click::ClickTool,
    cursor_tools::{
        GetAgentCursorStateV2Tool, SetAgentCursorEnabledV2Tool, SetAgentCursorMotionV2Tool,
        SetAgentCursorThemeTool,
    },
    debug_window_info::DebugWindowInfoTool,
    double_click::DoubleClickTool,
    drag::DragTool,
    get_accessibility_tree::GetAccessibilityTreeTool,
    get_config::GetConfigTool,
    get_cursor_position::GetCursorPositionTool,
    get_desktop_state::GetDesktopStateTool,
    get_screen_size::GetScreenSizeTool,
    get_window_state::GetWindowStateTool,
    hold_keys::HoldKeysTool,
    hotkey::HotkeyTool,
    input_timeline::InputTimelineTool,
    invoke_menu::InvokeMenuTool,
    kill_app::KillAppTool,
    launch_app::LaunchAppTool,
    list_apps::ListAppsTool,
    list_windows::ListWindowsTool,
    move_cursor::MoveCursorTool,
    press_key::PressKeyTool,
    right_click::RightClickTool,
    scroll::ScrollTool,
    set_config::SetConfigTool,
    set_value::SetValueTool,
    set_window_frame::SetWindowFrameTool,
    type_text::TypeTextTool,
    type_text_chars::TypeTextCharsTool,
    zoom::ZoomTool,
};

fn window_target_candidates_for_pid(
    windows: impl IntoIterator<Item = crate::win32::WindowInfo>,
    pid: u32,
) -> Vec<WindowTargetCandidate> {
    windows
        .into_iter()
        .filter(|window| window.pid == pid)
        .map(|window| WindowTargetCandidate {
            window_id: window.hwnd,
            transient_for: None,
            title: window.title,
            app_name: None,
            is_on_screen: window.is_on_screen,
        })
        .collect()
}

fn pid_window_target_candidates(pid: i64) -> Vec<WindowTargetCandidate> {
    let Ok(pid) = u32::try_from(pid) else {
        return Vec::new();
    };
    window_target_candidates_for_pid(crate::win32::list_windows_via_win32(Some(pid)), pid)
}

struct ExactPidWindowTargetGuard {
    inner: Box<dyn Tool>,
}

pub(crate) fn exact_window_ownership_result(
    pid: u32,
    window_id: u64,
    owner_pid: Option<u32>,
) -> Result<(), ToolResult> {
    match owner_pid {
        Some(owner_pid) if owner_pid == pid => Ok(()),
        Some(owner_pid) => Err(ToolResult::error(format!(
            "window_id {window_id} belongs to pid {owner_pid}, not pid {pid}."
        ))
        .with_structured(json!({
            "code": "window_target_mismatch",
            "effect": "refused",
            "pid": pid,
            "window_id": window_id,
            "owner_pid": owner_pid,
        }))),
        None => Err(ToolResult::error(format!(
            "window_id {window_id} is closed, stale, or invalid."
        ))
        .with_structured(json!({
            "code": "window_target_not_found",
            "effect": "refused",
            "pid": pid,
            "window_id": window_id,
        }))),
    }
}

#[async_trait]
impl Tool for ExactPidWindowTargetGuard {
    fn def(&self) -> &ToolDef {
        self.inner.def()
    }

    async fn protected_resource_ownership(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> cua_driver_core::tool::ProtectedResourceOwnership {
        self.inner
            .protected_resource_ownership(adapter_id, args)
            .await
    }

    async fn protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, cua_driver_core::tool::ProtectedResourceScopeError> {
        self.inner.protected_resource_scope(adapter_id, args).await
    }

    async fn validate_protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
        approved_scope: &Value,
    ) -> Result<(), cua_driver_core::tool::ProtectedResourceScopeError> {
        self.inner
            .validate_protected_resource_scope(adapter_id, args, approved_scope)
            .await
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        if args.get("scope").and_then(Value::as_str) != Some("desktop") {
            if let (Some(pid), Some(window_id)) = (
                args.get("pid").and_then(Value::as_u64),
                args.get("window_id").and_then(Value::as_u64),
            ) {
                let Ok(pid) = u32::try_from(pid) else {
                    return ToolResult::error("pid is outside the Windows process-id range.")
                        .with_structured(json!({
                            "code": "window_target_mismatch",
                            "effect": "refused",
                            "pid": pid,
                            "window_id": window_id,
                        }));
                };
                let owner_pid =
                    tokio::task::spawn_blocking(move || crate::win32::window_owner_pid(window_id))
                        .await
                        .ok()
                        .flatten();
                if let Err(result) = exact_window_ownership_result(pid, window_id, owner_pid) {
                    // Management refusals must retain their one-use source and false mutation prefix.
                    return if self.inner.def().name == "manage_window" {
                        window_management::bind_refusal(result, &args)
                    } else {
                        result
                    };
                }
            }
        }
        self.inner.invoke(args).await
    }
}

/// Return the human's foreground only with supported read evidence covering
/// every inserted event and no older unconfirmed prefix (`CUA_FOCUS_RETURN=1`).
/// The pid-only guard has already resolved the call's exact window.
struct FocusReturnGuard {
    inner: Box<dyn Tool>,
}

#[async_trait]
impl Tool for FocusReturnGuard {
    fn def(&self) -> &ToolDef {
        self.inner.def()
    }

    fn has_independent_input_lane(&self, args: &Value) -> bool {
        self.inner.has_independent_input_lane(args)
    }

    async fn protected_resource_ownership(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> cua_driver_core::tool::ProtectedResourceOwnership {
        self.inner
            .protected_resource_ownership(adapter_id, args)
            .await
    }

    async fn protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, cua_driver_core::tool::ProtectedResourceScopeError> {
        self.inner.protected_resource_scope(adapter_id, args).await
    }

    async fn validate_protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
        approved_scope: &Value,
    ) -> Result<(), cua_driver_core::tool::ProtectedResourceScopeError> {
        self.inner
            .validate_protected_resource_scope(adapter_id, args, approved_scope)
            .await
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let window = foreground_input_window(&self.inner.def().name, &args);
        let lease = match window {
            Some(window_id) if crate::focus_return::enabled() => {
                tokio::task::spawn_blocking(move || crate::focus_return::begin(window_id))
                    .await
                    .ok()
                    .flatten()
            }
            _ => None,
        };
        let before = crate::focus_return::unconfirmed_generation();
        let mut result = self.inner.invoke(args).await;
        let inserted_unconfirmed = crate::focus_return::unconfirmed_generation() != before;
        // Missing read knowledge is not permission to return unread global input
        // to the human. Even a kept lease must finish and remember the human.
        if !inserted_unconfirmed
            && !has_input_completion(&result)
            && result
                .structured_content
                .as_ref()
                .is_some_and(|value| value["effect"] == "refused")
        {
            result = crate::typing_turn::completion_result(
                result,
                crate::typing_turn::TypingTurn::NotSent,
            );
        }
        if inserted_unconfirmed && confirmed_input_read(&result) {
            result = crate::typing_turn::completion_result(
                result,
                crate::typing_turn::TypingTurn::Unobservable,
            );
        }
        let allow_restore = !inserted_unconfirmed && confirmed_input_read(&result);
        if !allow_restore && (window.is_some() || has_input_completion(&result)) {
            crate::focus_return::note_unconfirmed_input();
        }
        if (window.is_some() || inserted_unconfirmed)
            && !allow_restore
            && !has_input_completion(&result)
        {
            result = crate::typing_turn::completion_result(
                result,
                crate::typing_turn::TypingTurn::Unobservable,
            );
        }
        if let Some(lease) = lease {
            if let Ok(Some(line)) =
                tokio::task::spawn_blocking(move || lease.finish(allow_restore)).await
            {
                result
                    .content
                    .push(cua_driver_core::protocol::Content::text(line));
            }
        }
        result
    }
}

fn has_input_completion(result: &ToolResult) -> bool {
    result.input_read.is_some()
}

fn confirmed_input_read(result: &ToolResult) -> bool {
    matches!(
        result.input_read,
        Some(crate::typing_turn::TypingTurn::NotSent | crate::typing_turn::TypingTurn::Drained)
    )
}

/// Schema of the `uia_only` argument shared by `click` and `scroll`.
pub(crate) fn uia_only_schema() -> Value {
    json!({"type":"boolean","description":"With delivery_mode \"background\" and an element_token: act only through the element's UIA patterns. When no pattern carries the action the call refuses with background_unavailable before anything is sent: no window messages and no targeted injection. Points (x,y) refuse."})
}

/// `uia_only` acts on an element through UIA only, so it needs an element_token and background delivery.
pub(crate) fn uia_only_refusal(args: &Value, uia_only: bool) -> Option<ToolResult> {
    if !uia_only {
        return None;
    }
    if args.get("element_token").is_none_or(Value::is_null) {
        return Some(ToolResult::error(
            "uia_only acts on an element through UIA: give element_token, not x,y.",
        ));
    }
    if crate::input::delivery::DeliveryMode::from_args(args)
        == crate::input::delivery::DeliveryMode::Foreground
    {
        return Some(ToolResult::error(
            "uia_only applies to delivery_mode \"background\"; foreground delivery is real input.",
        ));
    }
    None
}

/// The refusal of a capture-bound action (click, drag, wheel, pointer move) whose capture
/// cannot be admitted.
pub(crate) fn capture_admission_refusal(error: anyhow::Error) -> ToolResult {
    let code = cua_driver_core::capture_runtime::admission_error_code(&error);
    crate::typing_turn::completion_result(
        ToolResult::error(format!("Capture-bound action refused: {error}")).with_structured(
            json!({
                "code": code,
                "effect": "refused",
                "detail": error.to_string(),
            }),
        ),
        crate::typing_turn::TypingTurn::NotSent,
    )
}

/// Input tools whose `delivery_mode:"foreground"` takes the foreground for their window.
/// `move_pointer` and `scroll_at` intentionally retain their measured posture;
/// their inserted input still records uncertainty through the shared generation.
const FOREGROUND_INPUT_TOOLS: &[&str] = &[
    "click",
    "double_click",
    "right_click",
    "drag",
    "hold_keys",
    "input_timeline",
    "type_text",
    "press_key",
    "hotkey",
    "scroll",
];

/// The exact window a foreground input call takes the foreground for, from its top-level
/// `window_id` or its `target`.
fn foreground_input_window(tool: &str, args: &Value) -> Option<u64> {
    if !FOREGROUND_INPUT_TOOLS.contains(&tool)
        || args.get("scope").and_then(Value::as_str) == Some("desktop")
        || crate::input::delivery::DeliveryMode::from_args(args)
            != crate::input::delivery::DeliveryMode::Foreground
    {
        return None;
    }
    args.get("window_id")
        .and_then(Value::as_u64)
        .or_else(|| args.get("target")?.get("window_id")?.as_u64())
}

type PidWindowGuardParts = (
    WindowTargetCandidates,
    cua_driver_core::window_target::TokenPidResolver,
);

fn pid_window_guarded<T: Tool + 'static>(
    tool: T,
    (candidates, token_pid_resolver): &PidWindowGuardParts,
) -> Box<dyn Tool> {
    Box::new(ExactPidWindowTargetGuard {
        inner: Box::new(
            PidOnlyWindowTargetGuard::new(
                Box::new(FocusReturnGuard {
                    inner: Box::new(tool),
                }),
                candidates.clone(),
            )
            .with_token_pid_resolver(token_pid_resolver.clone()),
        ),
    })
}

/// The pid an `element_token` was minted for, so a token-only action (no
/// `pid`) reaches its tool with the pid the token carries.
fn token_pid_resolver(state: Arc<ToolState>) -> cua_driver_core::window_target::TokenPidResolver {
    Arc::new(move |args| state.snapshots.token_owner(args))
}

/// Cursor sentinel for a direct platform call without lifecycle metadata.
/// Normal runtime dispatch supplies either a named or implicit session key.
pub(crate) const NO_CURSOR: &str = "";

/// Resolve the cursor key for a tool invocation, or [`NO_CURSOR`] (`""`) for an
/// anonymous call.
///
/// A cursor is tied to the lifecycle session selected at the trusted dispatch
/// boundary. Precedence: explicit `session`, trusted implicit `_session_id`,
/// then the legacy `cursor_id` alias. A direct platform invocation that has no
/// lifecycle metadata remains cursor-less.
pub(crate) fn resolve_cursor_key(args: &Value) -> String {
    cua_driver_core::tool_args::session_key(args).unwrap_or_else(|| NO_CURSOR.to_owned())
}

/// Returns `true` when a click/scroll invocation should take the **window-less
/// screen-absolute** branch: the caller gave numeric `x` AND `y`, gave NO `pid`
/// and NO `window_id`, and explicitly selected `scope="desktop"`.
///
/// Pure + arg-shape agnostic (works for both click and scroll args) so it's
/// unit-testable without Win32. When this returns `false` but `x,y` are present
/// with no pid/window_id, the caller returns a coordinate-scope error.
pub(crate) fn is_windowless_desktop_action(args: &serde_json::Value) -> bool {
    if args.get("scope").and_then(Value::as_str) != Some("desktop") {
        return false;
    }
    let has_pid = args.get("pid").map(|v| !v.is_null()).unwrap_or(false);
    let has_window_id = args.get("window_id").map(|v| !v.is_null()).unwrap_or(false);
    if has_pid || has_window_id {
        return false;
    }
    let has_num = |k: &str| args.get(k).map(|v| v.is_number()).unwrap_or(false);
    has_num("x") && has_num("y")
}

// ── DriverConfig + ZoomRegistry ─────────────────────────────────────────────

#[derive(Clone)]
pub struct DriverConfig {
    pub capture_mode: String,
    pub max_image_dimension: u32,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            capture_mode: "ax".into(),
            max_image_dimension: 1568,
        }
    }
}

/// Load `DriverConfig` from `~/.cua-driver/config.json`, falling back to
/// defaults for any missing/malformed keys. Called once at `ToolState`
/// construction (i.e. on every fresh `cua-driver call` process) so that a
/// prior `set_config max_image_dimension=...` survives across stateless
/// one-shot invocations — matching the macOS daemon's startup load. See #2008.
/// (`capture_mode` is still loaded/persisted for back-compat but no longer
/// affects behavior — `get_window_state` always returns tree + screenshot.)
pub fn load_driver_config() -> DriverConfig {
    let mut cfg = DriverConfig::default();
    if let Some(v) = cua_driver_core::config_file::read_config_value("capture_mode")
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        cfg.capture_mode = v;
    }
    if let Some(v) = cua_driver_core::config_file::read_config_value("max_image_dimension")
        .and_then(|v| v.as_u64())
    {
        if let Ok(v32) = u32::try_from(v) {
            cfg.max_image_dimension = v32;
        }
    }
    cfg
}

use cua_driver_core::snapshot_store::ZoomContext;

pub struct ToolState {
    pub snapshots: Arc<Snapshots>,
    pub cursor_registry: Arc<CursorRegistry>,
    pub config: Arc<RwLock<DriverConfig>>,
    pub(crate) capture_bridge: Option<Arc<crate::capture_admission::WindowsCaptureBridge>>,
    pub(crate) persistent_wgc: Arc<crate::persistent_wgc::Manager>,
    pub(crate) window_status: Arc<window_restore::Manager>,
}

impl ToolState {
    pub fn new(
        capture_service: Option<Arc<cua_driver_core::capture_runtime::CaptureService>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            snapshots: Arc::new(Snapshots::new()),
            cursor_registry: Arc::new(CursorRegistry::new()),
            config: Arc::new(RwLock::new(load_driver_config())),
            persistent_wgc: Arc::new(crate::persistent_wgc::Manager::new()),
            window_status: Arc::new(window_restore::Manager::new()),
            capture_bridge: capture_service
                .map(crate::capture_admission::WindowsCaptureBridge::new),
        })
    }

    pub(crate) fn zoom_context(
        &self,
        args: &Value,
        pid: u32,
        window_id: Option<u64>,
    ) -> Result<ZoomContext, ToolResult> {
        self.snapshots.zoom(
            pid as i32,
            window_id,
            args.get("_session_id").and_then(Value::as_str),
        )
    }
}

pub(crate) fn screenshot_scale(
    state: &ToolState,
    args: &Value,
    pid: u32,
    window_id: Option<u64>,
) -> Result<f64, ToolResult> {
    state
        .snapshots
        .screenshot_scale(pid as i32, window_id, args)
}

/// Split a `launch_path` round-tripped from `list_apps` into a single
/// ShellExecuteEx `lpFile` token plus a trailing arguments string. Handles
/// three input shapes:
///   - bare path (`C:\Windows\notepad.exe`): returns `(path, "")`
///   - quoted-exe + args (`"C:\Path\chrome.exe" --foo --bar`): returns the
///     unquoted path + the rest
///   - AUMID / shell launch token (`shell:appsFolder\..!..`): returns the
///     whole string as the file token with empty args (the shell resolver
///     handles these directly)
///
/// Args are returned trimmed; their internal quoting is preserved as-is so
/// `--profile-directory="Profile 2"` survives.
pub(crate) fn split_launchable_target(s: &str) -> (String, String) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    // AUMID / shell launch tokens go through unchanged.
    if trimmed.starts_with("shell:") || trimmed.contains('!') {
        return (trimmed.to_owned(), String::new());
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(close) = rest.find('"') {
            let path = &rest[..close];
            let args = rest[close + 1..].trim_start().to_owned();
            return (path.to_owned(), args);
        }
        // Unterminated quote — fall through and hand the whole thing back.
        return (trimmed.to_owned(), String::new());
    }
    // No quoting: be permissive. If the input contains a space and the
    // first whitespace-separated token names a `.exe`, treat it as
    // `<exe> <args>`. Otherwise hand the string back whole (lets bare paths
    // with embedded spaces still work — most installs put `Program Files`
    // shortcuts through the quoted branch above).
    if let Some(first_space) = trimmed.find(char::is_whitespace) {
        let (first, rest) = trimmed.split_at(first_space);
        if first.to_ascii_lowercase().ends_with(".exe") {
            return (first.to_owned(), rest.trim_start().to_owned());
        }
    }
    (trimmed.to_owned(), String::new())
}

/// Returns `true` when the launch target names a Chromium-based browser
/// — i.e. one whose hidden launch under `SW_SHOWNOACTIVATE` will trigger
/// renderer-occlusion throttling unless we inject the anti-throttling flags
/// below. See #1620.
///
/// Matches the executable basename (case-insensitive, with or without
/// `.exe`). Accepts bare names (the App Paths shortcut, e.g. `"msedge"`),
/// full paths (`"C:\...\msedge.exe"`), and round-tripped launch paths with
/// trailing arguments (`"\"C:\...\chrome.exe\" --foo"` — the first token is
/// matched via `split_launchable_target`).
pub(crate) fn is_chromium_browser_target(target: &str) -> bool {
    let (file, _) = split_launchable_target(target);
    let candidate = if file.is_empty() {
        target
    } else {
        file.as_str()
    };
    let lower = candidate.to_ascii_lowercase();
    let basename = lower
        .rsplit_once('\\')
        .map(|(_, b)| b)
        .or_else(|| lower.rsplit_once('/').map(|(_, b)| b))
        .unwrap_or(lower.as_str());
    let stem = basename.strip_suffix(".exe").unwrap_or(basename);
    matches!(
        stem,
        "msedge"
            | "chrome"
            | "brave"
            | "opera"
            | "vivaldi"
            | "chromium"
            | "thorium"
            | "iridium"
            | "browser" // Yandex Browser's exe is browser.exe
            | "arc"
    )
}

/// Async polling restore of the prior foreground window, used by
/// `LaunchAppTool` to mirror the macOS `FocusRestoreGuard` for the legacy
/// `ShellExecuteExW` launch path. The UWP/AUMID branch has its own
/// synchronous restoration in `launch_uwp::restore_foreground_best_effort`
/// and is NOT routed through here.
///
/// Approach:
/// 1. Try a short `WaitForInputIdle` on the spawned pid (if the handle
///    is openable) — this gets us past the worst of the new app's
///    window-creation window before we start sampling foreground state.
/// 2. Poll every 100ms for up to ~3s; when `GetForegroundWindow()` is no
///    longer the prior foreground (i.e. the spawned app actually grabbed
///    focus), call `SetForegroundWindow(prior)`. We avoid restoring
///    eagerly so we don't race the new app's own activation and lose.
///
/// `SetForegroundWindow` from non-UIAccess processes is restricted by
/// Windows' foreground-lock; the call may silently fail. We log the
/// failure at trace level and proceed — no error is surfaced, the launch
/// itself already succeeded.
///
/// Note: `prior_foreground_addr` is the raw HWND value as `usize` (rather
/// than `HWND` directly) because `HWND` wraps `*mut c_void` which is
/// `!Send` and would prevent this future from being scheduled on the
/// multi-threaded tokio runtime.
pub(crate) async fn restore_foreground_polling_best_effort(
    prior_foreground_addr: usize,
    spawned_pid: u32,
    input_activity: u64,
) {
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForInputIdle, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
    };

    if prior_foreground_addr == 0 {
        tracing::trace!(
            target: "launch_app.focus_restore",
            "no prior foreground HWND to restore — skipping"
        );
        return;
    }

    // Without a captured spawned pid we have no ownership signal — any
    // foreground change during the 3 s window might be the user
    // legitimately Alt-Tabbing, and we shouldn't yank focus back. Skip
    // the entire guard. This is the launcher-stub fallback case
    // (rare for fresh launches).
    if spawned_pid == 0 {
        tracing::trace!(
            target: "launch_app.focus_restore",
            "no spawned pid captured — skipping focus-restore guard (no ownership signal)"
        );
        return;
    }

    // Phase 1: best-effort wait for the spawned process to finish initial
    // window creation. WaitForInputIdle returns when the process pumps
    // its first message — that's typically when the main window has
    // registered. Capped at 1s; the polling loop below covers the rest.
    if spawned_pid != 0 {
        // PROCESS_SYNCHRONIZE is required by WaitForInputIdle;
        // PROCESS_QUERY_LIMITED_INFORMATION is a no-op extra that some
        // antivirus filters tolerate better than QUERY_INFORMATION.
        // OpenProcess result is also `!Send` (HANDLE), so do the open +
        // wait + close in one blocking task.
        let _ = tokio::task::spawn_blocking(move || unsafe {
            if let Ok(handle) = OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                spawned_pid,
            ) {
                if !handle.is_invalid() {
                    let _ = WaitForInputIdle(handle, 1000);
                }
                // Always close the handle, even if `is_invalid()` (cheap;
                // matches the OpenProcess pairing convention used elsewhere
                // in this crate).
                let _ = CloseHandle(handle);
            }
            // Silently no-op if OpenProcess failed (target may have already
            // exited; rare for a fresh launch but not impossible for a
            // launcher-stub).
        })
        .await;
    }

    // Phase 2: poll for "spawned app became foreground", then flip back.
    // Reconstruct HWND only inside synchronous probes so the future state
    // never holds an `!Send` value across an await point.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if std::time::Instant::now() >= deadline {
            tracing::trace!(
                target: "launch_app.focus_restore",
                "spawned pid {} did not steal foreground within deadline — no restore needed",
                spawned_pid
            );
            return;
        }
        // Only restore when the current foreground window is owned by
        // the spawned pid — otherwise the user may have legitimately
        // Alt-Tabbed during the polling window and we'd be yanking
        // focus away from them. Resolves fg_now's owning PID via
        // GetWindowThreadProcessId and compares against spawned_pid.
        // A launch poller must not steal the foreground from a later input
        // action or from a stream whose receiver read remains unconfirmed.
        let activated_then_restored: Option<bool> =
            cua_driver_core::input_lock::run_if_activity_unchanged(input_activity, || {
            if crate::focus_return::active() || crate::focus_return::has_unconfirmed_input() {
                return Some(true);
            }
            let prior = HWND(prior_foreground_addr as *mut _);
            let fg_now = unsafe { GetForegroundWindow() };
            if fg_now == prior {
                None
            } else {
                let mut fg_pid: u32 = 0;
                let _ = unsafe { GetWindowThreadProcessId(fg_now, Some(&mut fg_pid)) };
                if fg_pid != spawned_pid {
                    // Foreground changed but to something we didn't
                    // spawn (e.g. user Alt-Tabbed). Don't touch it.
                    None
                } else if !unsafe { IsWindow(prior) }.as_bool() {
                    tracing::trace!(
                        target: "launch_app.focus_restore",
                        "prior foreground HWND no longer valid — skipping restore (spawned pid {} owns fg)",
                        spawned_pid
                    );
                    Some(true)
                } else {
                    let ok = unsafe { SetForegroundWindow(prior) }.as_bool();
                    if !ok {
                        tracing::trace!(
                            target: "launch_app.focus_restore",
                            "SetForegroundWindow returned FALSE — likely foreground-lock denial; \
                             prior HWND 0x{:x}, spawned pid {}",
                            prior_foreground_addr,
                            spawned_pid
                        );
                    } else {
                        tracing::debug!(
                            target: "launch_app.focus_restore",
                            "restored prior foreground HWND 0x{:x} after spawned pid {} activated",
                            prior_foreground_addr,
                            spawned_pid
                        );
                    }
                    Some(ok)
                }
            }
        }).unwrap_or(Some(true));
        if activated_then_restored.is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// px-focus for the keyboard family (type_text / press_key / hotkey): pixel-click
/// at (x,y) to establish real renderer focus before a keystroke — the *element px
/// action* form of a keyboard tool. Reuses ClickTool's exact coordinate
/// translation + delivery_mode (UIA hit-test → PostMessage fallback), so it lands
/// on the same pixel a px-click would. `Ok(())` on success; `Err(ToolResult)`
/// short-circuits the caller. Mirrors macOS `tools::focus_by_pixel`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn focus_by_pixel_click_args(
    pid: u32,
    window_id: Option<u64>,
    x: f64,
    y: f64,
    foreground: bool,
    session: Option<String>,
    session_id: Option<String>,
    from_zoom: bool,
    native_coordinates: bool,
) -> Value {
    let mut click_args = json!({
        "pid": pid, "x": x, "y": y,
        "delivery_mode": if foreground { "foreground" } else { "background" },
    });
    if let Some(wid) = window_id {
        click_args["window_id"] = json!(wid);
    }
    if let Some(s) = session {
        click_args["session"] = json!(s);
    }
    if let Some(s) = session_id {
        click_args["_session_id"] = json!(s);
    }
    if from_zoom {
        click_args["from_zoom"] = json!(true);
    }
    if native_coordinates {
        click_args["_native_coordinates"] = json!(true);
    }
    click_args
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn focus_by_pixel(
    state: &Arc<ToolState>,
    pid: u32,
    window_id: Option<u64>,
    x: f64,
    y: f64,
    foreground: bool,
    session: Option<String>,
    session_id: Option<String>,
    from_zoom: bool,
    native_coordinates: bool,
) -> Result<(), ToolResult> {
    let click_args = focus_by_pixel_click_args(
        pid,
        window_id,
        x,
        y,
        foreground,
        session,
        session_id,
        from_zoom,
        native_coordinates,
    );
    let focus = ClickTool {
        state: state.clone(),
    }
    .invoke(click_args)
    .await;
    if focus.is_error == Some(true) {
        // Preserve the click tool's structured background refusal (for example
        // background_occluded / background_uipi_blocked). Re-wrapping it as a
        // text-only error made keyboard-family PX calls lose the actionable
        // capability result produced by the actual actuator.
        return Err(focus);
    }
    // Brief settle so the renderer registers focus before the keystrokes.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    Ok(())
}

fn wait_for_cached_element_keyboard_focus(
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if admitted
            .as_ref()
            .and_then(|element| element.element_has_keyboard_focus())
            == Some(true)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Establish exact UIA child focus after the owning top-level HWND is already
/// confirmed foreground. Chromium providers sometimes reject UIA `SetFocus`;
/// in that case a real click at the accessibility-derived center is the
/// bounded fallback. Both routes require `CurrentHasKeyboardFocus` read-back
/// before any keyboard input is allowed to leave the driver.
pub(crate) fn focus_cached_element_for_foreground(
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    hwnd: u64,
    element_index: usize,
    click_point: Option<(i32, i32)>,
) -> anyhow::Result<crate::typing_turn::TypingTurn> {
    // `fg_bypass` is a background-only shield: for Chromium it disables the
    // top-level HWND while UIA runs so the provider cannot self-activate. A
    // disabled foreground window immediately loses foreground on Windows,
    // which would make the enclosing verify-before-SendInput transaction
    // fail closed. The foreground route has already activated the exact HWND,
    // so focus the cached element directly and verify it below.
    let set_focus = admitted
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing admitted element"))
        .and_then(|element| element.focus_element());
    if set_focus.is_ok()
        && wait_for_cached_element_keyboard_focus(admitted, std::time::Duration::from_millis(350))
    {
        return Ok(crate::typing_turn::TypingTurn::NotSent);
    }

    if let Some((x, y)) = click_point {
        crate::input::send_click_synthesized_active_mods(hwnd, x, y, 1, "left", &[]).map_err(
            |error| crate::typing_turn::ForegroundInputError {
                error,
                turn: crate::typing_turn::TypingTurn::Unobservable,
            },
        )?;
        if wait_for_cached_element_keyboard_focus(admitted, std::time::Duration::from_millis(500)) {
            return Ok(crate::typing_turn::TypingTurn::Unobservable);
        }
        return Err(crate::typing_turn::ForegroundInputError {
            error: anyhow::anyhow!(
                "foreground_unavailable: focus click was inserted, but Windows did not confirm \
                 keyboard focus on exact UIA element [{element_index}] in HWND 0x{hwnd:x}; \
                 no keyboard input was sent"
            ),
            turn: crate::typing_turn::TypingTurn::Unobservable,
        }
        .into());
    }

    let detail = set_focus
        .err()
        .map(|error| format!("; UIA SetFocus failed: {error}"))
        .unwrap_or_default();
    anyhow::bail!(
        "foreground_unavailable: Windows did not confirm keyboard focus on exact UIA element \
         [{element_index}] in HWND 0x{hwnd:x}{detail}; no keyboard input was sent"
    )
}

// The standalone screenshot tools were removed in PR #1692.
// `get_window_state` is the canonical window-capture path and
// `get_desktop_state` handles full-display capture. The underlying functions
// (`crate::capture::screenshot_window_bytes_with_occlusion`,
// `screenshot_display_bytes`, etc.) and the WGC backend
// (`crate::wgc::screenshot_window_via_wgc`) are still in use by
// `GetWindowStateTool` — that's where the actual screenshot machinery
// lives now.

/// Refuse legacy non-activating global input on Chromium. A fixed wait cannot
/// safely restore foreground or z-order while the target may still have unread
/// pointer input. Other targets keep their fixed-HWND PostMessage route.
pub(crate) async fn chromium_click_short_circuit(
    hwnd: u64,
    _sx: i32,
    _sy: i32,
    _count: usize,
    _button: &str,
    _pid: u32,
    _gesture: &str,
) -> Option<ToolResult> {
    let is_chromium =
        tokio::task::spawn_blocking(move || crate::input::is_chromium_target_window(hwnd))
            .await
            .unwrap_or(false);
    is_chromium.then(|| {
        crate::input::delivery::background_unavailable_error_with_cause(
            hwnd,
            crate::input::delivery::EventKind::MouseClick,
            "non-activating global pointer input has no supported receiver-read witness; \
             no input was sent",
        )
    })
}

/// Fire UIA `InvokePattern.Invoke()` `count` times (min 2) in rapid succession
/// on a cached WinUI3 element. Two rapid Invokes land within the system
/// double-click time, so a Button's Click handler folds them into a
/// double-click (the harness records `last_action=double_click`).
///
/// This is the **only** background actuator that lands on a WinUI3 element: the
/// content island ignores posted `WM_*BUTTON` messages (measured: a posted
/// secondary click never fires `RightTapped`), and the synthetic pen/touch
/// injector click-activates the frame (8/8 foreground steals). UIA delivery
/// goes through the accessibility channel, not the input queue.
///
/// Contract: a XAML `Invoke` handler calls `SetFocus()` → `SetForegroundWindow(self)`,
/// which the `EnableWindow` UWP bypass does NOT gate (measured: a bare double
/// Invoke stole foreground). We therefore hold the frame **non-activatable**
/// (`WS_EX_NOACTIVATE`, via `NoActivateGuard`) for the whole burst, which is
/// what makes Windows refuse the self-activation — the same guard ClickTool
/// already relies on for single-click. Runs blocking COM, so call inside
/// `spawn_blocking`. Returns `Some(Ok)` on success, `Some(Err)` if Invoke
/// failed, `None` if the element isn't cached or has no InvokePattern.
fn winui3_uia_multi_invoke(
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    hwnd: u64,
    count: usize,
) -> Option<anyhow::Result<()>> {
    let guard = admitted.as_ref().filter(|element| element.is_uia())?;
    let ptr = guard.as_ptr();
    use windows::core::Interface;
    use windows::Win32::UI::Accessibility::{
        IUIAutomationElement, IUIAutomationInvokePattern, UIA_InvokePatternId,
    };
    // Hold the frame non-activatable so the XAML Invoke handler's
    // SetFocus()→SetForegroundWindow(self) is refused for the whole burst.
    let _noact =
        crate::input::NoActivateGuard::arm(windows::Win32::Foundation::HWND(hwnd as *mut _));
    let elem: IUIAutomationElement = unsafe { IUIAutomationElement::from_raw(ptr as *mut _) };
    let outcome: Option<anyhow::Result<()>> = (|| {
        let pattern = unsafe { elem.GetCurrentPattern(UIA_InvokePatternId) }.ok()?;
        let inv = pattern.cast::<IUIAutomationInvokePattern>().ok()?;
        let n = count.max(2);
        let mut res = Ok(());
        for i in 0..n {
            let r = crate::uia::fg_bypass::run_with_uwp_bypass(hwnd as isize, || unsafe {
                inv.Invoke()
            });
            if let Err(e) = r {
                res = Err(anyhow::anyhow!("UIA Invoke #{i}: {e}"));
                break;
            }
            if i + 1 < n {
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
        }
        Some(res)
    })();
    // The cache owns this element's ref (AddRef'd under the cache lock by
    // get_element_retained); `from_raw` took ownership without AddRef, so forget
    // it to avoid double-releasing the cache's ref.
    std::mem::forget(elem);
    outcome
}

/// WinUI3 background gesture resolution, shared by click / double_click /
/// right_click / drag. For a WinUI3 (`WinUIDesktopWin32WindowClass`) target:
///
///  - **double-click (left)** on a cached InvokePattern element → a double UIA
///    Invoke (lands + holds the no-foreground contract; see
///    [`winui3_uia_multi_invoke`]).
///  - **everything else** — right/middle click, double-click without an
///    invokable cached element, any drag — cannot both land AND hold the
///    contract on WinUI3: UIA has no right-click/drag analogue, the content
///    island ignores posted `WM_*BUTTON`, and the pointer injector
///    click-activates the frame. So we return the structured
///    `background_unavailable` error (the honest result — the caller can retry
///    that action with `delivery_mode:"foreground"`) instead of a silent
///    no-op that reports false success.
///
/// Returns `None` for non-WinUI3 targets (caller falls through to its normal
/// routing).
pub(crate) async fn winui3_background_gesture(
    admitted: &Option<crate::uia::snapshot::RetainedElement>,
    pid: u32,
    hwnd: u64,
    idx: Option<usize>,
    count: usize,
    button: &str,
) -> Option<ToolResult> {
    let is_w = tokio::task::spawn_blocking({
        let admitted = admitted.clone();
        move || {
            let _admission = &admitted;
            crate::input::delivery::is_winui3_target_window(hwnd)
        }
    })
    .await
    .unwrap_or(false);
    if !is_w {
        return None;
    }
    if count >= 2 && button == "left" {
        if let Some(idx) = idx {
            let uia = tokio::task::spawn_blocking({
                let admitted = admitted.clone();
                move || {
                    let _admission = &admitted;
                    winui3_uia_multi_invoke(&admitted, hwnd, count)
                }
            })
            .await
            .ok()
            .flatten();
            if let Some(Ok(())) = uia {
                return Some(ToolResult::text(format!(
                    "✅ Double-invoked WinUI3 element [{idx}] x{count} via UIA \
                     (pid {pid}, background, no foreground swap)."
                )));
            }
        }
    }
    Some(crate::input::delivery::background_unavailable_error(
        hwnd,
        crate::input::delivery::EventKind::MouseClick,
    ))
}

/// Read the primary display size in PHYSICAL pixels.
///
/// With permonitorv2 DPI awareness (set in cua-driver.manifest),
/// `SM_CXSCREEN` / `SM_CYSCREEN` already return physical pixels — the same
/// coordinate space screenshots and pixel clicks use on Windows. Shared by
/// `get_screen_size` and `get_desktop_state`.
pub(crate) fn physical_screen_size() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

// ── registry builder ──────────────────────────────────────────────────────────

pub fn build_registry_with_provider(
    compat: bool,
    provider: Option<std::sync::Arc<dyn cua_driver_core::consent::ProtectedConsentProvider>>,
) -> ToolRegistry {
    let mut r = ToolRegistry::new_with_protected_consent_provider(provider);
    let state = ToolState::new(Some(r.capture_service()));
    {
        let state = state.clone();
        r.set_token_window_resolver(Arc::new(move |args| state.snapshots.window_for_token(args)));
    }
    let cursor_outcome_reader = {
        let cursor_registry = state.cursor_registry.clone();
        cua_driver_core::session::register_scoped_cursor_outcome_reader(std::sync::Arc::new(
            move |session_id| {
                let state = cursor_registry.get(session_id);
                let motion_customized = state.is_some()
                    && crate::overlay::current_motion(session_id)
                        != cursor_overlay::MotionConfig::default();
                let active_cursor_count = cursor_registry
                    .all_states()
                    .iter()
                    .filter(|state| state.config.cursor_id != "default")
                    .count()
                    .max(1);
                match state {
                    Some(state) => cua_driver_core::session::bounded_cursor_outcome(
                        true,
                        state.config.enabled,
                        crate::overlay::is_visible_for_session(session_id),
                        Some(state.config.theme_id.as_str()),
                        motion_customized,
                        active_cursor_count,
                    ),
                    None => cua_driver_core::session::bounded_cursor_outcome(
                        false,
                        false,
                        false,
                        None,
                        false,
                        active_cursor_count,
                    ),
                }
            },
        ))
    };
    // Share the element cache with the recording-hook layer so it can
    // resolve element_index → window-local screenshot coords for click.png.
    crate::recording_hooks::set_snapshots(state.snapshots.clone());
    crate::recording_hooks::install();

    // Drop a session's owned cursor on `session_end` (explicit end_session, the
    // CLI `session end` verb, or the daemon idle-TTL sweep). The session id IS
    // the cursor key (caller-declared `session`), so this prunes the metadata
    // registry AND stops the overlay painting that session's cursor. Both paths
    // guard "default" so the anonymous / one-shot cursor survives. The scoped
    // registration lets each direct runtime clean up its own cursor state and
    // deregisters when that runtime's registry is dropped. Mirrors the macOS
    // `register_all` session_end hook (platform-macos/src/tools/mod.rs).
    let cursor_registry = state.cursor_registry.clone();
    let snapshots = state.snapshots.clone();
    let persistent_wgc = state.persistent_wgc.clone();
    let capture_bridge = state.capture_bridge.clone();
    let session_end_hook =
        cua_driver_core::session::register_scoped_session_end_hook(move |session_id| {
            snapshots.retire_session_screenshots(session_id);
            persistent_wgc.close_session(session_id);
            if let Some(bridge) = &capture_bridge {
                bridge.close_session(session_id);
            }
            cursor_registry.remove(session_id);
            crate::overlay::remove_cursor(session_id.to_owned());
        });
    let session_revive_hook =
        cua_driver_core::session::register_scoped_session_revive_hook(move |session_id| {
            crate::overlay::revive_cursor(session_id.to_owned());
        });

    r.retain_cursor_outcome_reader(cursor_outcome_reader);
    r.retain_session_end_hook(session_end_hook);
    r.retain_session_revive_hook(session_revive_hook);
    if let Some(runtime_scope) = cua_driver_core::tool::current_dispatch_runtime_scope() {
        let prefix = format!("__cua_runtime_{runtime_scope}:");
        let cursor_registry = state.cursor_registry.clone();
        r.retain_runtime_cleanup(move || {
            for cursor in cursor_registry
                .all_states()
                .into_iter()
                .filter(|cursor| cursor.config.cursor_id.starts_with(&prefix))
            {
                cursor_registry.remove(&cursor.config.cursor_id);
                crate::overlay::remove_cursor(cursor.config.cursor_id);
            }
        });
    }
    r.register(Box::new(ListAppsTool));
    r.register(Box::new(ListWindowsTool));
    r.register(Box::new(GetWindowStateTool {
        state: state.clone(),
    }));
    r.register(Box::new(
        cua_driver_core::expectation::VerifyStateTool::new(std::sync::Arc::new(
            cua_driver_core::expectation::ToolObservationProvider::new(
                std::sync::Arc::new(ListWindowsTool),
                std::sync::Arc::new(GetWindowStateTool {
                    state: state.clone(),
                }),
            ),
        )),
    ));
    r.register(Box::new(LaunchAppTool));
    r.register(Box::new(KillAppTool));
    let pid_window_candidates: PidWindowGuardParts = (
        Arc::new(pid_window_target_candidates),
        token_pid_resolver(state.clone()),
    );
    r.register(pid_window_guarded(BringToFrontTool, &pid_window_candidates));
    r.register(pid_window_guarded(
        window_restore::GetWindowStatusTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        window_restore::RestoreWindowTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        window_management::GetWindowControlStateTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        window_management::ManageWindowTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(Box::new(SetWindowFrameTool));
    r.register(Box::new(InvokeMenuTool));
    r.register(Box::new(DebugWindowInfoTool));
    r.register(pid_window_guarded(
        ClickTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        DoubleClickTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        RightClickTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        HoldKeysTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        DragTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        InputTimelineTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        TypeTextTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        PressKeyTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        HotkeyTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        super::semantic::SemanticActionTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        SetValueTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        ScrollTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    cua_driver_core::clipboard::register_clipboard_tools(
        &mut r,
        Arc::new(crate::clipboard::WindowsClipboard::new()),
    );
    // The standalone screenshot tools were removed from the tool surface.
    // `get_window_state` is the canonical path for a window screenshot and
    // `get_desktop_state` handles explicit full-display capture. Reasons:
    //   1. One entry point means callers always have a window_id context,
    //      which `screenshot` made ambiguous (whole-display fallback
    //      always picked the wrong thing for headless agent workflows).
    //   2. `get_window_state` always returns a PNG alongside the tree
    //      — there's no information `screenshot` exposed that's not in
    //      `get_window_state`.
    //   3. Claude Code's vision pipeline can be retargeted at
    //      `get_window_state` via its tool-anchor config — no need for
    //      a dedicated "screenshot" name.
    // The `capture::*` functions remain the underlying screenshot machinery.
    let _ = compat; // legacy flag; no current tool-surface effect
    r.register(Box::new(GetScreenSizeTool));
    r.register(Box::new(super::displays::ListDisplaysTool));
    r.register(Box::new(super::displays::GetDisplayStateTool));
    r.register(Box::new(GetDesktopStateTool {
        state: state.clone(),
    }));
    r.register(Box::new(GetCursorPositionTool));
    r.register(pid_window_guarded(
        pointer::MovePointerTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(pid_window_guarded(
        scroll::ScrollAtTool {
            state: state.clone(),
        },
        &pid_window_candidates,
    ));
    r.register(Box::new(MoveCursorTool {
        state: state.clone(),
    }));
    r.register(Box::new(SetAgentCursorEnabledV2Tool {
        state: state.clone(),
    }));
    r.register(Box::new(SetAgentCursorMotionV2Tool));
    r.register(Box::new(activity::SetAgentVisualsTool));
    r.register(Box::new(activity::PresentAgentActivityTool));
    r.register(Box::new(GetAgentCursorStateV2Tool {
        state: state.clone(),
    }));
    r.register(Box::new(SetAgentCursorThemeTool {
        state: state.clone(),
    }));
    r.register(Box::new(CheckPermissionsTool));
    // `health_report` — single-call cross-platform driver diagnostics.
    // Stable schema_version="1" contract for downstream consumers. Windows skips
    // tcc_* and bundle_identity with "not applicable on Windows".
    r.register(Box::new(
        cua_driver_core::health_report::HealthReportTool::new(std::sync::Arc::new(
            crate::health_report::WindowsHealthProvider,
        )),
    ));
    r.register(Box::new(GetConfigTool {
        state: state.clone(),
    }));
    r.register(Box::new(SetConfigTool {
        state: state.clone(),
    }));
    r.register(Box::new(GetAccessibilityTreeTool));
    r.register(Box::new(ZoomTool {
        state: state.clone(),
    }));
    // `type_text_chars` is intentionally NOT registered — Swift treats it
    // as a deprecated alias for `type_text` resolved at invoke time in
    // mcp-server's `ToolRegistry::invoke`. Keeping it out of the registry
    // means it doesn't show up in `tools/list` either, matching Swift's
    // ToolRegistry.swift (`type_text_chars` not in `handlers`).
    let _: &TypeTextCharsTool = &TypeTextCharsTool {
        _state: state.clone(),
    }; // touch struct so it stays in this crate for now
       // Cross-platform `page` tool definition lives in mcp-server; Windows plugs
       // in its UIA TextPattern + FindAll backend.
    r.register(Box::new(cua_driver_core::page::PageTool::new(
        std::sync::Arc::new(super::page::WindowsPageBackend::new()),
    )));
    let browser_engine = cua_driver_core::browser::BrowserEngine::new_with_runtime_services(
        std::sync::Arc::new(crate::browser_platform::WindowsBrowserPlatform::new(
            state.cursor_registry.clone(),
        )),
        r.approval_broker(),
        r.protected_resource_ownership(),
    );
    cua_driver_core::browser::register_browser_tools(&browser_engine, &mut r);
    r.register_recording_tools();
    r.register_session_tools();
    r
}

#[cfg(test)]
mod drag_capture_id_tests;
#[cfg(test)]
mod semantic_admission_tests;
#[cfg(test)]
mod uia_only_tests;

#[cfg(test)]
mod cursor_key_resolution_tests {
    use super::{resolve_cursor_key, NO_CURSOR};
    use serde_json::json;

    /// Precedence is core `tool_args::session_key`'s contract; Windows only
    /// owns the cursor-less fallback for a direct call without lifecycle
    /// metadata.
    #[test]
    fn anonymous_direct_call_is_cursor_less() {
        assert_eq!(resolve_cursor_key(&json!({ "pid": 1 })), NO_CURSOR);
        assert_eq!(
            resolve_cursor_key(&json!({ "session": "", "cursor_id": "" })),
            NO_CURSOR
        );
        assert_eq!(
            resolve_cursor_key(&json!({ "_session_id": "mcp-1-2" })),
            "mcp-1-2"
        );
    }
}

#[cfg(test)]
mod pid_window_target_tests {
    use super::*;
    use cua_driver_core::window_target::{resolve_pid_window_target, PidWindowTargetResolution};

    fn window(hwnd: u64, pid: u32) -> crate::win32::WindowInfo {
        crate::win32::WindowInfo {
            hwnd,
            pid,
            title: format!("Document {hwnd}"),
            x: 0,
            y: 0,
            width: 640,
            height: 480,
            is_on_screen: true,
            minimized: false,
        }
    }

    #[test]
    fn same_pid_sibling_windows_are_ambiguous() {
        let candidates =
            window_target_candidates_for_pid([window(7, 42), window(8, 42), window(9, 99)], 42);
        assert!(matches!(
            resolve_pid_window_target(candidates),
            PidWindowTargetResolution::Ambiguous(windows)
                if windows.iter().map(|window| window.window_id).collect::<Vec<_>>() == [7, 8]
        ));
    }
}
