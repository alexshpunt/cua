// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Enumerate top-level windows on Windows.
//!
//! Win32 enumeration retains its z-order and immersive-shell coverage. UIA
//! contributes additional HWNDs after the native entries, without reordering
//! them. A bounded single-flight UIA worker falls back to the native list if
//! a provider is slow or unavailable.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, GetAncestor, GetClassNameW, GetDesktopWindow,
    GetWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, GA_PARENT, GA_ROOT, GW_OWNER,
};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// HWND cast to u64 for serialization.
    pub hwnd: u64,
    /// pid owning the window.
    pub pid: u32,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub is_on_screen: bool,
    pub minimized: bool,
}

struct EnumState {
    windows: Vec<WindowInfo>,
}

/// List native windows in z-order, then UIA-only windows. If `filter_pid`
/// is Some, only that process's windows.
pub fn list_windows(filter_pid: Option<u32>) -> Vec<WindowInfo> {
    let win32_windows = merge_top_level_windows(
        enumerate_via_enum_windows(),
        top_level_handles(),
        immersive_window_info,
    );
    merge_uia_windows(
        win32_windows,
        cached_uia_windows()
            .into_iter()
            .filter_map(revalidate_uia_window)
            .collect(),
        filter_pid,
    )
}

// The listing path never waits for UIA. A fresh result can supplement later
// calls; the first call and stale results fall back to Win32 alone.
const UIA_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const UIA_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct UiaWindowCache {
    last_started: Option<Instant>,
    snapshot: Option<(Instant, Vec<WindowInfo>)>,
    in_flight: bool,
}

fn cached_uia_windows() -> Vec<WindowInfo> {
    static CACHE: OnceLock<Arc<Mutex<UiaWindowCache>>> = OnceLock::new();
    request_uia_windows(
        CACHE.get_or_init(|| Arc::new(Mutex::new(UiaWindowCache::default()))),
        crate::uia::windows_enum::enumerate_top_level_windows,
    )
}

fn request_uia_windows(
    cache: &Arc<Mutex<UiaWindowCache>>,
    enumerate: impl FnOnce() -> Vec<WindowInfo> + Send + 'static,
) -> Vec<WindowInfo> {
    let now = Instant::now();
    // A concurrent refresh must never make listing wait on a UIA provider.
    let Ok(mut state) = cache.try_lock() else {
        return Vec::new();
    };
    let snapshot = state
        .snapshot
        .as_ref()
        .filter(|(started, _)| now.saturating_duration_since(*started) <= UIA_SNAPSHOT_MAX_AGE)
        .map(|(_, windows)| windows.clone())
        .unwrap_or_default();
    let due = state
        .last_started
        .is_none_or(|started| now.saturating_duration_since(started) >= UIA_REFRESH_INTERVAL);
    if !state.in_flight && due {
        state.in_flight = true;
        state.last_started = Some(now);
        let worker_cache = Arc::clone(cache);
        if std::thread::Builder::new()
            .name("cua-uia-window-cache".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(enumerate));
                if let Ok(mut state) = worker_cache.lock() {
                    state.in_flight = false;
                    // A failed refresh must not resurrect an earlier snapshot.
                    state.snapshot = result.ok().map(|windows| (now, windows));
                }
            })
            .is_err()
        {
            state.in_flight = false;
            state.snapshot = None;
        }
    }
    snapshot
}

/// Re-read identity and mutable properties before exposing a cached UIA HWND.
fn revalidate_uia_window(window: WindowInfo) -> Option<WindowInfo> {
    let hwnd = HWND(window.hwnd as *mut _);
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
        || unsafe { IsIconic(hwnd) }.as_bool()
        || window_is_cloaked(hwnd)
    {
        return None;
    }
    let current = window_info_by_handle(window.hwnd)?;
    let owner_after = window_owner_pid(window.hwnd);
    validate_uia_snapshot(window, current, owner_after)
}

fn validate_uia_snapshot(
    snapshot: WindowInfo,
    current: WindowInfo,
    owner_after: Option<u32>,
) -> Option<WindowInfo> {
    (current.hwnd == snapshot.hwnd
        && current.pid == snapshot.pid
        && current.width > 0
        && current.height > 0
        && owner_after == Some(snapshot.pid))
    .then_some(current)
}

/// Keep native z-order, then append UIA-only HWNDs without duplicates.
fn merge_uia_windows(
    mut windows: Vec<WindowInfo>,
    uia_windows: Vec<WindowInfo>,
    filter_pid: Option<u32>,
) -> Vec<WindowInfo> {
    let mut seen: HashSet<u64> = windows.iter().map(|window| window.hwnd).collect();
    windows.extend(
        uia_windows
            .into_iter()
            .filter(|window| seen.insert(window.hwnd)),
    );
    if let Some(pid) = filter_pid {
        windows.retain(|window| window.pid == pid);
    }
    windows
}

/// Append the `handles` that `enum_windows` skipped and `probe` accepts,
/// keeping the `EnumWindows` order first.
fn merge_top_level_windows(
    enum_windows: Vec<WindowInfo>,
    handles: Vec<u64>,
    probe: impl Fn(u64) -> Option<WindowInfo>,
) -> Vec<WindowInfo> {
    let seen: HashSet<u64> = enum_windows.iter().map(|w| w.hwnd).collect();
    let mut merged = enum_windows;
    merged.extend(
        handles
            .into_iter()
            .filter(|hwnd| !seen.contains(hwnd))
            .filter_map(probe),
    );
    merged
}

/// Every top-level HWND on this desktop, including the immersive windows
/// `EnumWindows` leaves out.
fn top_level_handles() -> Vec<u64> {
    let mut handles = Vec::new();
    let mut after = HWND::default();
    while let Ok(next) = unsafe { FindWindowExW(None, Some(after), PCWSTR::null(), PCWSTR::null()) }
    {
        if next.0.is_null() {
            break;
        }
        handles.push(next.0 as u64);
        after = next;
    }
    handles
}

/// A window `EnumWindows` skipped, kept only while it is actually shown:
/// visible, titled, not minimized and not cloaked.
fn immersive_window_info(hwnd: u64) -> Option<WindowInfo> {
    let native = HWND(hwnd as *mut _);
    if unsafe { IsIconic(native) }.as_bool() || window_is_cloaked(native) {
        return None;
    }
    window_info_by_handle(hwnd)
}

fn window_is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_err()
            || cloaked != 0
    }
}

/// Resolve one exact Win32 window without enumerating the desktop.
pub(crate) fn find_window_by_pid_and_handle(pid: u32, hwnd: u64) -> Option<WindowInfo> {
    exact_window_from_probe(pid, hwnd, window_info_by_handle)
}

/// Outcome of validating a caller-supplied `(pid, window_id)` pair.
#[derive(Debug, Clone)]
pub(crate) enum PidWindowLookup {
    /// The HWND is a visible, titled top-level window owned by `pid`.
    Found(WindowInfo),
    /// The HWND is listed, but under another pid.
    OtherPid(u32),
    /// No listed window has this HWND.
    Missing,
}

/// Validate that `hwnd` is a listed top-level window of `pid`.
///
/// Probes the exact HWND first. Only a miss falls back to the full listing,
/// which resolves immersive windows and names the owning pid of a
/// mismatched HWND.
pub(crate) fn lookup_window_for_pid(pid: u32, hwnd: u64) -> PidWindowLookup {
    lookup_window_for_pid_with(pid, hwnd, find_window_by_pid_and_handle, || {
        list_windows(None)
    })
}

fn lookup_window_for_pid_with(
    pid: u32,
    hwnd: u64,
    exact: impl FnOnce(u32, u64) -> Option<WindowInfo>,
    full: impl FnOnce() -> Vec<WindowInfo>,
) -> PidWindowLookup {
    if let Some(window) = exact(pid, hwnd) {
        return PidWindowLookup::Found(window);
    }
    match full().into_iter().find(|window| window.hwnd == hwnd) {
        Some(window) if window.pid == pid => PidWindowLookup::Found(window),
        Some(window) => PidWindowLookup::OtherPid(window.pid),
        None => PidWindowLookup::Missing,
    }
}

/// List `pid`'s top-level windows, consulting UIA only when Win32 finds none.
///
/// Polling callers (e.g. `launch_app` waiting for a first window) use the
/// native fast path without starting a UIA refresh when it already finds one.
/// A UIA-only window may appear on a later poll after the refresh completes.
pub(crate) fn list_windows_win32_first(pid: u32) -> Vec<WindowInfo> {
    win32_first_with(
        || list_windows_via_win32(Some(pid)),
        || list_windows(Some(pid)),
    )
}

fn win32_first_with(
    win32: impl FnOnce() -> Vec<WindowInfo>,
    full: impl FnOnce() -> Vec<WindowInfo>,
) -> Vec<WindowInfo> {
    let windows = win32();
    if windows.is_empty() {
        full()
    } else {
        windows
    }
}

fn exact_window_from_probe(
    pid: u32,
    hwnd: u64,
    probe: impl FnOnce(u64) -> Option<WindowInfo>,
) -> Option<WindowInfo> {
    probe(hwnd).filter(|window| window.pid == pid && window.hwnd == hwnd)
}

/// Return the native owner of one exact HWND without enumerating Win32 or UIA.
pub(crate) fn window_owner_pid(hwnd: u64) -> Option<u32> {
    let hwnd = HWND(hwnd as *mut _);
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    let mut pid = 0;
    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (thread_id != 0 && pid != 0).then_some(pid)
}

#[derive(Clone, Copy, Debug)]
struct PostActionForegroundRelation {
    exact_match: bool,
    target_identity_live: bool,
    target_dismissed: bool,
    actual_live: bool,
    actual_visible: bool,
    same_pid: bool,
    ownership_reaches_target: bool,
    /// The actual window's parent chain reaches the target: a window of any
    /// process embedded inside it, such as a Unity Editor in a Herdr tab.
    embedded_in_target: bool,
    actual_is_prior_owner: bool,
    click_dismissed_unowned_target: bool,
}

fn post_action_foreground_allowed(relation: PostActionForegroundRelation) -> bool {
    relation.click_dismissed_unowned_target
        || (relation.actual_live
            && relation.actual_visible
            && ((relation.target_identity_live
                && ((relation.same_pid
                    && (relation.exact_match || relation.ownership_reaches_target))
                    || relation.embedded_in_target))
                || (relation.target_dismissed
                    && relation.same_pid
                    && relation.actual_is_prior_owner)))
}

/// A target is dismissed when destroyed, or when the same window is hidden after a click.
fn target_dismissed(current_pid: Option<u32>, snapshot_pid: u32, hidden: bool) -> bool {
    match current_pid {
        None => true,
        Some(pid) => pid == snapshot_pid && hidden,
    }
}

/// Whether following `next` from `actual` (its owner or its parent) reaches
/// `target`, within 64 steps and without a cycle back to `actual`.
fn chain_reaches_target(
    target: u64,
    actual: u64,
    mut next: impl FnMut(u64) -> Option<u64>,
) -> bool {
    let mut current = actual;
    for _ in 0..64 {
        let Some(step) = next(current) else {
            return false;
        };
        if step == target {
            return true;
        }
        if step == current || step == actual {
            return false;
        }
        current = step;
    }
    false
}

fn owner_of(hwnd: u64) -> Option<u64> {
    let owner = unsafe { GetWindow(HWND(hwnd as *mut _), GW_OWNER) }
        .ok()
        .unwrap_or_default();
    (!owner.0.is_null()).then_some(owner.0 as usize as u64)
}

/// The window's parent as `GetAncestor(GA_PARENT)` reports it; none for a
/// top-level window, whose parent is the desktop. A popup that another
/// process moved into its own window with `SetParent` keeps its popup style,
/// and `GetParent` returns its owner instead; `GA_PARENT` names the parent.
fn parent_of(hwnd: u64) -> Option<u64> {
    let parent = unsafe { GetAncestor(HWND(hwnd as *mut _), GA_PARENT) };
    (!parent.0.is_null() && parent != unsafe { GetDesktopWindow() })
        .then_some(parent.0 as usize as u64)
}

/// Whether `actual` is a live, visible window other than `target` whose
/// parent chain reaches `target`, whatever process it belongs to: a window
/// embedded in the target, such as a Unity Editor in a Herdr tab. While such
/// a window has the foreground, keyboard input to the target belongs to it.
pub(crate) fn embedded_in_target(target: u64, actual: u64) -> bool {
    target != actual
        && window_owner_pid(target).is_some()
        && window_owner_pid(actual).is_some()
        && unsafe { IsWindowVisible(HWND(actual as *mut _)) }.as_bool()
        && chain_reaches_target(target, actual, parent_of)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ForegroundTarget {
    hwnd: u64,
    pid: u32,
    owner: Option<u64>,
}

impl ForegroundTarget {
    /// Identity only, including a hidden window still completing deactivation.
    /// This does not authorize a new DOWN or a foreground restoration.
    pub(crate) fn is_same_live_window(self, actual: u64) -> bool {
        self.hwnd == actual && window_owner_pid(actual) == Some(self.pid)
    }
}

/// Snapshot the exact target identity and owner before global input is sent.
pub(crate) fn capture_foreground_target(target: u64) -> Option<ForegroundTarget> {
    let pid = window_owner_pid(target)?;
    let owner = unsafe { GetWindow(HWND(target as *mut _), GW_OWNER) }
        .ok()
        .filter(|owner| !owner.0.is_null())
        .map(|owner| owner.0 as usize as u64);
    Some(ForegroundTarget {
        hwnd: target,
        pid,
        owner,
    })
}

/// Verify the foreground before or after an exact-target global input action.
///
/// The requested HWND must become foreground before input is sent. The action
/// may then legitimately open a same-process owned popup or modal, so the
/// post-action check accepts that transient only when it is still a live,
/// visible HWND and its complete `GW_OWNER` chain reaches the exact target.
/// A click may also give the foreground to a window of another process that
/// is embedded in the target (its parent chain reaches it), which counts as
/// the target.
/// A dismiss action may instead destroy the requested owned modal; that is
/// accepted only when foreground returns to its snapshotted same-process owner.
/// Unrelated same-process siblings and foreign foreground windows fail closed.
pub(crate) fn foreground_matches_target_or_owned_window(
    target: ForegroundTarget,
    actual: u64,
) -> bool {
    foreground_matches(target, actual, false)
}

/// [`foreground_matches_target_or_owned_window`] after a click, which may also
/// dismiss the owned modal by hiding it. A click that destroys or hides an
/// unowned target leaves no owner for the foreground to return to, so Windows
/// may activate any window; the dismissal is the click's observed effect.
pub(crate) fn foreground_after_click_matches(target: ForegroundTarget, actual: u64) -> bool {
    foreground_matches(target, actual, true)
}

/// Whether `hwnd` is destroyed or hidden.
pub(crate) fn window_closed_or_hidden(hwnd: u64) -> bool {
    let native = HWND(hwnd as *mut _);
    !unsafe { IsWindow(Some(native)) }.as_bool() || !unsafe { IsWindowVisible(native) }.as_bool()
}

fn foreground_matches(target: ForegroundTarget, actual: u64, after_click: bool) -> bool {
    let current_target_pid = window_owner_pid(target.hwnd);
    let actual_pid = window_owner_pid(actual);
    let actual_hwnd = HWND(actual as *mut _);
    let actual_visible = actual_pid.is_some() && unsafe { IsWindowVisible(actual_hwnd) }.as_bool();
    let ownership_reaches_target =
        target.hwnd != actual && chain_reaches_target(target.hwnd, actual, owner_of);
    let embedded_in_target =
        target.hwnd != actual && chain_reaches_target(target.hwnd, actual, parent_of);

    let target_dismissed = target_dismissed(
        current_target_pid,
        target.pid,
        after_click && !unsafe { IsWindowVisible(HWND(target.hwnd as *mut _)) }.as_bool(),
    );

    post_action_foreground_allowed(PostActionForegroundRelation {
        exact_match: target.hwnd == actual,
        target_identity_live: current_target_pid == Some(target.pid),
        target_dismissed,
        actual_live: actual_pid.is_some(),
        actual_visible,
        same_pid: actual_pid == Some(target.pid),
        ownership_reaches_target,
        embedded_in_target,
        actual_is_prior_owner: target.owner == Some(actual),
        click_dismissed_unowned_target: after_click && target_dismissed && target.owner.is_none(),
    })
}

#[derive(Clone, Copy, Debug)]
struct ClosedTargetRelation {
    /// The target is destroyed, reused by another process or hidden.
    target_closed: bool,
    actual_is_target: bool,
    actual_live: bool,
    actual_visible: bool,
    same_pid: bool,
}

fn closed_target_successor_allowed(relation: ClosedTargetRelation) -> bool {
    relation.target_closed
        && !relation.actual_is_target
        && relation.actual_live
        && relation.actual_visible
        && relation.same_pid
}

/// After an action closed its target, such as OK or Cancel in a dialog: the
/// window now in front is a visible window of the target's process. The target
/// counts as closed once it is destroyed or hidden; toolkits such as GTK hide a
/// dialog before they destroy it. Only for the check after input was sent:
/// before input the exact target must be in front.
pub(crate) fn foreground_after_target_closed(target: ForegroundTarget, actual: u64) -> bool {
    let target_live = window_owner_pid(target.hwnd) == Some(target.pid);
    let target_visible =
        target_live && unsafe { IsWindowVisible(HWND(target.hwnd as *mut _)) }.as_bool();
    let actual_pid = window_owner_pid(actual);
    closed_target_successor_allowed(ClosedTargetRelation {
        target_closed: !target_visible,
        actual_is_target: actual == target.hwnd,
        actual_live: actual_pid.is_some(),
        actual_visible: actual_pid.is_some()
            && unsafe { IsWindowVisible(HWND(actual as *mut _)) }.as_bool(),
        same_pid: actual_pid == Some(target.pid),
    })
}

fn window_info_by_handle(hwnd: u64) -> Option<WindowInfo> {
    let native = HWND(hwnd as *mut _);
    let pid = window_owner_pid(hwnd)?;
    if !unsafe { IsWindowVisible(native) }.as_bool() {
        return None;
    }
    let title_len = unsafe { GetWindowTextLengthW(native) };
    if title_len == 0 {
        return None;
    }
    let mut buf = vec![0u16; (title_len + 1) as usize];
    unsafe { GetWindowTextW(native, &mut buf) };
    let title_len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let title = String::from_utf16_lossy(&buf[..title_len]);
    if title.trim().is_empty() {
        return None;
    }
    let minimized = unsafe { IsIconic(native) }.as_bool();
    let (x, y, width, height) = get_window_bounds(native);
    Some(WindowInfo {
        hwnd,
        pid,
        title,
        x,
        y,
        width,
        height,
        is_on_screen: !minimized,
        minimized,
    })
}

pub(crate) fn list_windows_via_win32(filter_pid: Option<u32>) -> Vec<WindowInfo> {
    let mut windows = enumerate_via_enum_windows();
    if let Some(pid) = filter_pid {
        windows.retain(|window| window.pid == pid);
    }
    windows
}

/// Walk `EnumWindows` and collect every visible, non-empty-titled
/// top-level window. No pid filter is applied here — the caller does that on
/// the merged list.
fn enumerate_via_enum_windows() -> Vec<WindowInfo> {
    let state = Mutex::new(EnumState {
        windows: Vec::new(),
    });
    let state_ptr = &state as *const Mutex<EnumState> as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_windows_cb), LPARAM(state_ptr));
    }
    state.into_inner().unwrap().windows
}

unsafe extern "system" fn enum_windows_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &*(lparam.0 as *const Mutex<EnumState>);

    // Invisible helper windows are not user-addressable. Iconic windows are:
    // retain them with explicit state so callers can restore them.
    if IsWindowVisible(hwnd).0 == 0 {
        return TRUE;
    }
    let minimized = IsIconic(hwnd).0 != 0;

    // Get pid.
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));

    // Get title (skip empty).
    let title_len = GetWindowTextLengthW(hwnd);
    if title_len == 0 {
        return TRUE;
    }
    let mut buf = vec![0u16; (title_len + 1) as usize];
    GetWindowTextW(hwnd, &mut buf);
    let title = {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    };
    if title.trim().is_empty() {
        return TRUE;
    }

    // Get bounds — prefer DWM extended frame bounds (includes shadow), fallback to GetWindowRect.
    let (x, y, w, h) = get_window_bounds(hwnd);

    state.lock().unwrap().windows.push(WindowInfo {
        hwnd: hwnd.0 as u64,
        pid,
        title,
        x,
        y,
        width: w,
        height: h,
        is_on_screen: !minimized,
        minimized,
    });

    TRUE
}

fn get_window_bounds(hwnd: HWND) -> (i32, i32, i32, i32) {
    unsafe {
        let mut rect = RECT::default();
        // Try DwmGetWindowAttribute for accurate bounds (excludes drop shadow on W11).
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if ok.is_err() {
            // Fallback to GetWindowRect.
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        (
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        )
    }
}

#[cfg(test)]
mod exact_window_tests {
    use super::{
        chain_reaches_target, closed_target_successor_allowed, exact_window_from_probe,
        lookup_window_for_pid_with, post_action_foreground_allowed, win32_first_with,
        ClosedTargetRelation, PidWindowLookup, PostActionForegroundRelation, WindowInfo,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn window(pid: u32, hwnd: u64) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            title: "Exact target".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            is_on_screen: true,
            minimized: false,
        }
    }

    #[test]
    fn pid_window_lookup_skips_full_enumeration_on_exact_hit() {
        let found = lookup_window_for_pid_with(
            42,
            0x1234,
            |pid, hwnd| Some(window(pid, hwnd)),
            || panic!("exact hit must not enumerate the desktop"),
        );
        assert!(matches!(found, PidWindowLookup::Found(w) if w.hwnd == 0x1234 && w.pid == 42));
    }

    #[test]
    fn pid_window_lookup_falls_back_for_uia_only_window() {
        let calls = AtomicUsize::new(0);
        let found = lookup_window_for_pid_with(
            42,
            0x1234,
            |_, _| None,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![window(7, 0x9999), window(42, 0x1234)]
            },
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(found, PidWindowLookup::Found(w) if w.hwnd == 0x1234));
    }

    #[test]
    fn pid_window_lookup_reports_other_pid_and_missing() {
        let other = lookup_window_for_pid_with(42, 0x1234, |_, _| None, || vec![window(7, 0x1234)]);
        assert!(matches!(other, PidWindowLookup::OtherPid(7)));

        let missing = lookup_window_for_pid_with(42, 0x1234, |_, _| None, Vec::new);
        assert!(matches!(missing, PidWindowLookup::Missing));
    }

    #[test]
    fn win32_first_listing_consults_uia_only_when_win32_is_empty() {
        let listed = win32_first_with(
            || vec![window(42, 0x1)],
            || panic!("Win32 hit must not enumerate UIA"),
        );
        assert_eq!(listed.len(), 1);

        let calls = AtomicUsize::new(0);
        let listed = win32_first_with(Vec::new, || {
            calls.fetch_add(1, Ordering::SeqCst);
            vec![window(42, 0x2)]
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(listed[0].hwnd, 0x2);
    }

    #[test]
    fn exact_lookup_probes_only_the_requested_native_handle() {
        let calls = AtomicUsize::new(0);
        let found = exact_window_from_probe(42, 0x1234, |hwnd| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(hwnd, 0x1234);
            Some(window(42, hwnd))
        });

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(found.map(|window| window.hwnd), Some(0x1234));
    }

    #[test]
    fn exact_lookup_rejects_wrong_pid_or_handle() {
        assert!(exact_window_from_probe(42, 7, |_| Some(window(43, 7))).is_none());
        assert!(exact_window_from_probe(42, 7, |_| Some(window(42, 8))).is_none());
    }

    fn relation(
        exact_match: bool,
        same_pid: bool,
        ownership_reaches_target: bool,
    ) -> PostActionForegroundRelation {
        PostActionForegroundRelation {
            exact_match,
            target_identity_live: true,
            target_dismissed: false,
            actual_live: true,
            actual_visible: true,
            same_pid,
            ownership_reaches_target,
            embedded_in_target: false,
            actual_is_prior_owner: false,
            click_dismissed_unowned_target: false,
        }
    }

    #[test]
    fn a_window_of_another_process_embedded_in_the_target_counts_as_the_target() {
        // A popup parented into a container that is a child of the target.
        let parents = [(30, 20), (20, 10)];
        assert!(chain_reaches_target(10, 30, |hwnd| {
            parents
                .iter()
                .find_map(|(child, parent)| (*child == hwnd).then_some(*parent))
        }));
        let mut embedded = relation(false, false, false);
        embedded.embedded_in_target = true;
        assert!(post_action_foreground_allowed(embedded));
        // Not once the target is gone, nor for a hidden or destroyed window.
        embedded.target_identity_live = false;
        embedded.target_dismissed = true;
        assert!(!post_action_foreground_allowed(embedded));
        embedded.target_identity_live = true;
        embedded.target_dismissed = false;
        embedded.actual_visible = false;
        assert!(!post_action_foreground_allowed(embedded));
        embedded.actual_visible = true;
        embedded.actual_live = false;
        assert!(!post_action_foreground_allowed(embedded));
        // A foreign window outside the target stays denied.
        assert!(!post_action_foreground_allowed(relation(
            false, false, false
        )));
    }

    #[test]
    fn exact_or_owned_modal_foreground_is_allowed() {
        assert!(post_action_foreground_allowed(relation(true, true, false)));
        assert!(post_action_foreground_allowed(relation(false, true, true)));
    }

    #[test]
    fn nested_owned_popup_chain_reaches_exact_target() {
        let owners = [(30, 20), (20, 10)];
        assert!(chain_reaches_target(10, 30, |hwnd| {
            owners
                .iter()
                .find_map(|(child, owner)| (*child == hwnd).then_some(*owner))
        }));
    }

    #[test]
    fn unrelated_same_pid_sibling_and_foreign_foreground_are_denied() {
        assert!(!post_action_foreground_allowed(relation(
            false, true, false
        )));
        assert!(!post_action_foreground_allowed(relation(
            false, false, true
        )));
        assert!(!chain_reaches_target(10, 30, |hwnd| {
            (hwnd == 30).then_some(40)
        }));
    }

    #[test]
    fn dismissed_owned_modal_may_return_to_its_snapshotted_owner_only() {
        let mut dismissed = relation(false, true, false);
        dismissed.target_identity_live = false;
        dismissed.target_dismissed = true;
        dismissed.actual_is_prior_owner = true;
        assert!(post_action_foreground_allowed(dismissed));

        dismissed.actual_is_prior_owner = false;
        assert!(!post_action_foreground_allowed(dismissed));

        dismissed.actual_is_prior_owner = true;
        dismissed.same_pid = false;
        assert!(!post_action_foreground_allowed(dismissed));
    }

    #[test]
    fn a_same_process_window_may_take_the_foreground_after_the_target_closed() {
        let closed = ClosedTargetRelation {
            target_closed: true,
            actual_is_target: false,
            actual_live: true,
            actual_visible: true,
            same_pid: true,
        };
        assert!(closed_target_successor_allowed(closed));
        // The target still shows: an unrelated sibling in front stays unverified.
        assert!(!closed_target_successor_allowed(ClosedTargetRelation {
            target_closed: false,
            ..closed
        }));
        // Another process in front stays unverified.
        assert!(!closed_target_successor_allowed(ClosedTargetRelation {
            same_pid: false,
            ..closed
        }));
        assert!(!closed_target_successor_allowed(ClosedTargetRelation {
            actual_visible: false,
            ..closed
        }));
        assert!(!closed_target_successor_allowed(ClosedTargetRelation {
            actual_live: false,
            ..closed
        }));
        assert!(!closed_target_successor_allowed(ClosedTargetRelation {
            actual_is_target: true,
            ..closed
        }));
    }

    #[test]
    fn stale_reused_invisible_or_cyclic_foreground_is_denied() {
        let mut stale = relation(false, true, true);
        stale.target_identity_live = false;
        assert!(!post_action_foreground_allowed(stale));
        stale.target_identity_live = true;
        stale.actual_live = false;
        assert!(!post_action_foreground_allowed(stale));
        stale.actual_live = true;
        stale.actual_visible = false;
        assert!(!post_action_foreground_allowed(stale));
        assert!(!chain_reaches_target(10, 30, |_| Some(30)));
    }
}

fn window_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Resolve a packaged-app (UWP) process id to the `ApplicationFrameWindow`
/// that actually hosts it.
///
/// ## Why this exists
///
/// `IApplicationActivationManager::ActivateApplication` (see `launch_uwp`)
/// returns the **real** packaged-app pid — e.g. `CalculatorApp.exe`,
/// `SystemSettings.exe`. But a modern UWP app's *top-level* window is not
/// owned by that process. `ApplicationFrameHost.exe` owns the top-level
/// `ApplicationFrameWindow` (title bar, caption, the HWND a user drags); the
/// app's own process only owns a `Windows.UI.Core.CoreWindow` reparented
/// *inside* that frame as a child. Consequences:
///
/// - `list_windows(Some(app_pid))` is **empty** — the app process owns no
///   top-level window, and the frame's `GetWindowThreadProcessId` reports the
///   AFH pid, not `app_pid`.
/// - One `ApplicationFrameHost.exe` pid hosts **many** unrelated UWP apps, so
///   the AFH pid alone is not an app identity — only the specific frame HWND
///   disambiguates.
///
/// So after a UWP launch, the handles a caller must actually drive are
/// `(frame_hwnd, afh_pid)` — NOT the `(app_pid, …)` pair `launch_app` would
/// otherwise report, which resolves to no window at all.
///
/// ## How the mapping is made
///
/// The stable identity link is process ownership of the hosted child: AFH
/// reparents the app's `CoreWindow` under the frame, and that child window's
/// `GetWindowThreadProcessId` reports the **app** pid (the child stays owned
/// by the app process even though it lives under the AFH frame). We therefore
/// walk every visible top-level `ApplicationFrameWindow`, scan its child
/// windows, and return the first frame that owns a child whose process id
/// equals `app_pid`. This keys off OS-level window parentage rather than a raw
/// HWND/pid the caller cached, so it stays correct across the HWND churn UWP
/// activations exhibit in the first moments after launch.
///
/// Returns `None` if no hosting frame is found yet (the frame can lag the
/// process by a few hundred ms — callers should retry) or if `app_pid` is 0
/// (brokered activations that report no pid; not resolvable by this path).
pub fn resolve_uwp_host_window(app_pid: u32) -> Option<WindowInfo> {
    if app_pid == 0 {
        return None;
    }

    struct FrameScan {
        /// pid we're hunting for among each frame's child windows.
        target_app_pid: u32,
        /// Set to the hosting frame's HWND once a child match is found.
        matched_frame: Option<HWND>,
    }

    // Outer pass: every top-level ApplicationFrameWindow. For each, an inner
    // EnumChildWindows pass looks for a child owned by `target_app_pid`.
    unsafe extern "system" fn frame_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let scan = &mut *(lparam.0 as *mut FrameScan);

        if IsWindowVisible(hwnd).0 == 0 || IsIconic(hwnd).0 != 0 {
            return TRUE;
        }
        // Only ApplicationFrameHost frames host UWP CoreWindows; skip the rest
        // cheaply before paying for a child enumeration.
        if window_class_name(hwnd) != "ApplicationFrameWindow" {
            return TRUE;
        }

        // Inner pass: does this frame own a child window belonging to the
        // launched app process?
        struct ChildScan {
            target_app_pid: u32,
            found: bool,
        }
        unsafe extern "system" fn child_cb(child: HWND, lparam: LPARAM) -> BOOL {
            let cs = &mut *(lparam.0 as *mut ChildScan);
            let mut child_pid: u32 = 0;
            GetWindowThreadProcessId(child, Some(&mut child_pid));
            if child_pid == cs.target_app_pid {
                cs.found = true;
                return windows::Win32::Foundation::FALSE; // stop enumerating children
            }
            TRUE
        }

        let mut child_scan = ChildScan {
            target_app_pid: scan.target_app_pid,
            found: false,
        };
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(child_cb),
            LPARAM(&mut child_scan as *mut ChildScan as isize),
        );

        if child_scan.found {
            scan.matched_frame = Some(hwnd);
            return windows::Win32::Foundation::FALSE; // stop enumerating frames
        }
        TRUE
    }

    let mut scan = FrameScan {
        target_app_pid: app_pid,
        matched_frame: None,
    };
    unsafe {
        let _ = EnumWindows(Some(frame_cb), LPARAM(&mut scan as *mut FrameScan as isize));
    }

    let frame = scan.matched_frame?;

    // Build the WindowInfo for the frame itself: its HWND is what the caller
    // drives, and its owning pid is the AFH pid (what get_window_state will
    // validate `window_id` against and what list_windows reports for it).
    let mut afh_pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(frame, Some(&mut afh_pid)) };

    let title = unsafe {
        let len = GetWindowTextLengthW(frame);
        if len == 0 {
            String::new()
        } else {
            let mut buf = vec![0u16; (len + 1) as usize];
            GetWindowTextW(frame, &mut buf);
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..n])
        }
    };

    let (x, y, w, h) = get_window_bounds(frame);

    Some(WindowInfo {
        hwnd: frame.0 as u64,
        pid: afh_pid,
        title,
        x,
        y,
        width: w,
        height: h,
        is_on_screen: true,
        minimized: false,
    })
}

/// Resolve the real packaged-app process hosted by a specific
/// `ApplicationFrameWindow`.
///
/// An ApplicationFrameHost pid is shared by unrelated apps, so callers must
/// provide the frame HWND as well as the host pid. The hosted CoreWindow child
/// retains ownership by the packaged app process and supplies the identity we
/// need for attribution.
pub fn resolve_uwp_app_pid(host_pid: u32, frame_hwnd: u64) -> Option<u32> {
    let frame = HWND(frame_hwnd as *mut _);
    if unsafe { IsWindow(Some(frame)) }.0 == 0
        || window_class_name(frame) != "ApplicationFrameWindow"
    {
        return None;
    }

    let mut owner_pid = 0;
    unsafe { GetWindowThreadProcessId(frame, Some(&mut owner_pid)) };
    if owner_pid != host_pid {
        return None;
    }

    struct ChildScan {
        host_pid: u32,
        app_pid: Option<u32>,
    }

    unsafe extern "system" fn child_cb(child: HWND, lparam: LPARAM) -> BOOL {
        let scan = &mut *(lparam.0 as *mut ChildScan);
        let mut child_pid = 0;
        GetWindowThreadProcessId(child, Some(&mut child_pid));
        if child_pid != 0
            && child_pid != scan.host_pid
            && window_class_name(child) == "Windows.UI.Core.CoreWindow"
        {
            scan.app_pid = Some(child_pid);
            return windows::Win32::Foundation::FALSE;
        }
        TRUE
    }

    let mut scan = ChildScan {
        host_pid,
        app_pid: None,
    };
    unsafe {
        let _ = EnumChildWindows(
            Some(frame),
            Some(child_cb),
            LPARAM(&mut scan as *mut ChildScan as isize),
        );
    }
    scan.app_pid
}

/// The packaged app a frame hosts, for capability-manifest identity.
///
/// Only the system's own `ApplicationFrameHost.exe` is trusted to host apps,
/// so another process cannot borrow an app's identity by giving its own
/// window the frame class and reparenting that app's window under it.
pub(crate) fn hosted_app_pid(host_pid: u32, frame_hwnd: u64) -> Option<u32> {
    let host = crate::history::process_executable_path(host_pid)?;
    let trusted = crate::browser_platform::system_directory_path()
        .ok()?
        .join("ApplicationFrameHost.exe");
    if !host
        .to_string_lossy()
        .eq_ignore_ascii_case(&trusted.to_string_lossy())
    {
        return None;
    }
    resolve_uwp_app_pid(host_pid, frame_hwnd)
}

#[cfg(test)]
mod upstream_exact_window_tests {
    use super::{
        chain_reaches_target, exact_window_from_probe, lookup_window_for_pid_with,
        merge_top_level_windows, merge_uia_windows, post_action_foreground_allowed,
        target_dismissed, PidWindowLookup, PostActionForegroundRelation, WindowInfo,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn window(pid: u32, hwnd: u64) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            title: "Exact target".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            is_on_screen: true,
            minimized: false,
        }
    }

    #[test]
    fn pid_window_lookup_skips_full_enumeration_on_exact_hit() {
        let found = lookup_window_for_pid_with(
            42,
            0x1234,
            |pid, hwnd| Some(window(pid, hwnd)),
            || panic!("exact hit must not enumerate the desktop"),
        );
        assert!(matches!(found, PidWindowLookup::Found(w) if w.hwnd == 0x1234 && w.pid == 42));
    }

    #[test]
    fn pid_window_lookup_falls_back_for_a_window_only_the_full_listing_finds() {
        let calls = AtomicUsize::new(0);
        let found = lookup_window_for_pid_with(
            42,
            0x1234,
            |_, _| None,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![window(7, 0x9999), window(42, 0x1234)]
            },
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(found, PidWindowLookup::Found(w) if w.hwnd == 0x1234));
    }

    #[test]
    fn pid_window_lookup_reports_other_pid_and_missing() {
        let other = lookup_window_for_pid_with(42, 0x1234, |_, _| None, || vec![window(7, 0x1234)]);
        assert!(matches!(other, PidWindowLookup::OtherPid(7)));

        let missing = lookup_window_for_pid_with(42, 0x1234, |_, _| None, Vec::new);
        assert!(matches!(missing, PidWindowLookup::Missing));
    }

    #[test]
    fn uia_cache_never_waits_for_a_stalled_provider_and_serves_completed_results() {
        use super::{request_uia_windows, UiaWindowCache};
        use std::sync::{mpsc, Arc, Mutex};
        use std::time::{Duration, Instant};

        let cache = Arc::new(Mutex::new(UiaWindowCache::default()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let before = Instant::now();
        assert!(request_uia_windows(&cache, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            vec![window(1, 0x30)]
        })
        .is_empty());
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(request_uia_windows(&cache, || panic!("duplicate refresh")).is_empty());
        assert!(before.elapsed() < Duration::from_secs(1));
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let complete = cache.lock().unwrap().snapshot.is_some();
            if complete {
                break;
            }
            assert!(Instant::now() < deadline, "cache worker did not finish");
            std::thread::yield_now();
        }
        let result = request_uia_windows(&cache, Vec::new);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hwnd, 0x30);
    }

    #[test]
    fn uia_cache_rejects_expired_snapshot_and_changed_window_identity() {
        use super::{
            request_uia_windows, validate_uia_snapshot, UiaWindowCache, UIA_SNAPSHOT_MAX_AGE,
        };
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let cache = Arc::new(Mutex::new(UiaWindowCache {
            last_started: Some(Instant::now()),
            snapshot: Some((
                Instant::now() - UIA_SNAPSHOT_MAX_AGE - Duration::from_millis(1),
                vec![window(1, 0x30)],
            )),
            in_flight: true,
        }));
        assert!(request_uia_windows(&cache, || panic!("duplicate refresh")).is_empty());
        assert!(validate_uia_snapshot(window(1, 0x30), window(2, 0x30), Some(2)).is_none());
        assert!(validate_uia_snapshot(window(1, 0x30), window(1, 0x40), Some(1)).is_none());
        assert!(validate_uia_snapshot(window(1, 0x30), window(1, 0x30), Some(2)).is_none());
        assert!(validate_uia_snapshot(window(1, 0x30), window(1, 0x30), Some(1)).is_some());
    }

    #[test]
    fn uia_only_windows_follow_native_order_and_filter_after_deduplication() {
        let native = vec![window(1, 0x30), window(2, 0x10)];
        let uia = vec![window(9, 0x30), window(1, 0x20), window(1, 0x20)];
        let merged = merge_uia_windows(native.clone(), uia.clone(), None);
        let order: Vec<_> = merged.iter().map(|w| (w.hwnd, w.pid)).collect();
        assert_eq!(order, [(0x30, 1), (0x10, 2), (0x20, 1)]);

        let filtered = merge_uia_windows(native, uia, Some(9));
        assert!(
            filtered.is_empty(),
            "a duplicate must not replace native ownership"
        );
    }

    #[test]
    fn skipped_top_level_windows_are_appended_after_enum_windows_order() {
        let merged = merge_top_level_windows(
            vec![window(1, 0x30), window(2, 0x10)],
            vec![0x10, 0x20, 0x30, 0x40, 0x50],
            |hwnd| (hwnd != 0x40).then(|| window(3, hwnd)),
        );
        let order: Vec<_> = merged.iter().map(|w| (w.hwnd, w.pid)).collect();
        assert_eq!(order, [(0x30, 1), (0x10, 2), (0x20, 3), (0x50, 3)]);
    }

    #[test]
    fn exact_lookup_probes_only_the_requested_native_handle() {
        let calls = AtomicUsize::new(0);
        let found = exact_window_from_probe(42, 0x1234, |hwnd| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(hwnd, 0x1234);
            Some(window(42, hwnd))
        });

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(found.map(|window| window.hwnd), Some(0x1234));
    }

    #[test]
    fn exact_lookup_rejects_wrong_pid_or_handle() {
        assert!(exact_window_from_probe(42, 7, |_| Some(window(43, 7))).is_none());
        assert!(exact_window_from_probe(42, 7, |_| Some(window(42, 8))).is_none());
    }

    fn relation(
        exact_match: bool,
        same_pid: bool,
        ownership_reaches_target: bool,
    ) -> PostActionForegroundRelation {
        PostActionForegroundRelation {
            exact_match,
            target_identity_live: true,
            target_dismissed: false,
            actual_live: true,
            actual_visible: true,
            same_pid,
            ownership_reaches_target,
            embedded_in_target: false,
            actual_is_prior_owner: false,
            click_dismissed_unowned_target: false,
        }
    }

    #[test]
    fn exact_or_owned_modal_foreground_is_allowed() {
        assert!(post_action_foreground_allowed(relation(true, true, false)));
        assert!(post_action_foreground_allowed(relation(false, true, true)));
    }

    #[test]
    fn nested_owned_popup_chain_reaches_exact_target() {
        let owners = [(30, 20), (20, 10)];
        assert!(chain_reaches_target(10, 30, |hwnd| {
            owners
                .iter()
                .find_map(|(child, owner)| (*child == hwnd).then_some(*owner))
        }));
    }

    #[test]
    fn unrelated_same_pid_sibling_and_foreign_foreground_are_denied() {
        assert!(!post_action_foreground_allowed(relation(
            false, true, false
        )));
        assert!(!post_action_foreground_allowed(relation(
            false, false, true
        )));
        assert!(!chain_reaches_target(10, 30, |hwnd| {
            (hwnd == 30).then_some(40)
        }));
    }

    #[test]
    fn dismissed_owned_modal_may_return_to_its_snapshotted_owner_only() {
        let mut dismissed = relation(false, true, false);
        dismissed.target_identity_live = false;
        dismissed.target_dismissed = true;
        dismissed.actual_is_prior_owner = true;
        assert!(post_action_foreground_allowed(dismissed));

        dismissed.actual_is_prior_owner = false;
        assert!(!post_action_foreground_allowed(dismissed));

        dismissed.actual_is_prior_owner = true;
        dismissed.same_pid = false;
        assert!(!post_action_foreground_allowed(dismissed));
    }

    #[test]
    fn click_that_dismisses_an_unowned_target_accepts_any_foreground() {
        let mut closed = relation(false, false, false);
        closed.target_identity_live = false;
        closed.target_dismissed = true;
        closed.actual_live = false;
        closed.actual_visible = false;
        assert!(!post_action_foreground_allowed(closed));
        closed.click_dismissed_unowned_target = true;
        assert!(post_action_foreground_allowed(closed));
    }

    #[test]
    fn destroyed_or_hidden_same_target_counts_as_dismissed() {
        assert!(target_dismissed(None, 42, false));
        assert!(target_dismissed(Some(42), 42, true));
        assert!(!target_dismissed(Some(42), 42, false));
        assert!(!target_dismissed(Some(43), 42, true));
    }

    #[test]
    fn stale_reused_invisible_or_cyclic_foreground_is_denied() {
        let mut stale = relation(false, true, true);
        stale.target_identity_live = false;
        assert!(!post_action_foreground_allowed(stale));
        stale.target_identity_live = true;
        stale.actual_live = false;
        assert!(!post_action_foreground_allowed(stale));
        stale.actual_live = true;
        stale.actual_visible = false;
        assert!(!post_action_foreground_allowed(stale));
        assert!(!chain_reaches_target(10, 30, |_| Some(30)));
    }
}

#[cfg(test)]
mod hung_window_tests {
    use super::list_windows;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW, MSG,
        PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    const HOST_ENV: &str = "CUA_TEST_HUNG_WINDOW_HOST";
    const TITLE: &str = "cua list_windows hung window";

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Child-process half of the test below: shows a titled window, then
    /// stops pumping messages so it stays hung until the parent kills it.
    #[test]
    #[ignore = "spawned by list_windows_does_not_wait_on_a_hung_window"]
    fn hung_window_host() {
        if std::env::var_os(HOST_ENV).is_none() {
            return;
        }
        unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("CuaListWindowsHungHost"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0, "register window class");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class.lpszClassName,
                &windows::core::HSTRING::from(TITLE),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                100,
                100,
                400,
                300,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("create hung window");
            let pumped = Instant::now();
            let mut msg = MSG::default();
            while pumped.elapsed() < Duration::from_millis(500) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            println!("hwnd={}", hwnd.0 as u64);
        }
        std::thread::sleep(Duration::from_secs(120));
    }

    struct KillOnDrop(Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// #4906: listing windows must not wait on another process's window
    /// that has stopped answering messages, and must still list it.
    #[test]
    #[ignore = "shows a window on the interactive desktop; ci-rust-windows.yml selects it"]
    fn list_windows_does_not_wait_on_a_hung_window() {
        let host_test = module_path!()
            .split_once("::")
            .map(|(_, path)| format!("{path}::hung_window_host"))
            .expect("crate-qualified module path");
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([host_test.as_str(), "--exact", "--ignored", "--nocapture"])
            .env(HOST_ENV, "1")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn hung window host");
        let stdout = child.stdout.take().expect("host stdout");
        let _host = KillOnDrop(child);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(hwnd) = line.strip_prefix("hwnd=") {
                    let _ = tx.send(hwnd.trim().parse::<u64>().expect("hwnd"));
                    return;
                }
            }
        });
        let hung = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("host window created");

        let started = Instant::now();
        let windows = list_windows(None);
        let elapsed = started.elapsed();

        let listed = windows
            .iter()
            .find(|w| w.hwnd == hung)
            .expect("the hung window is listed");
        assert_eq!(listed.title, TITLE);
        assert!(
            elapsed < Duration::from_secs(1),
            "list_windows took {elapsed:?} with a hung window on the desktop"
        );
    }
}
