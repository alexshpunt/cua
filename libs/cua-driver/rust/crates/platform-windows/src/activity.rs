//! Session-owned human-only presentation. The worker has no input/capture operations.

use cursor_overlay::activity::{ActivityEvent, ActivityKind, VisualSettings};
use cursor_overlay::{CursorAction, OverlayCommand};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_SESSIONS: usize = 64;
const MAX_PENDING_SOURCES: usize = 4;
#[derive(Default)]
struct Session {
    settings: VisualSettings,
    generation: u64,
    pending: Option<ActivityEvent>,
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

pub fn settings(key: &str) -> VisualSettings {
    state()
        .lock()
        .ok()
        .and_then(|s| s.sessions.get(key).map(|s| s.settings.clone()))
        .unwrap_or_default()
}

pub fn configure(key: &str, patch: serde_json::Value) -> Result<VisualSettings, &'static str> {
    if key.is_empty() || !crate::overlay::draws_cursor(key) {
        return Err("visual_session_unavailable");
    }
    let mut state = state().lock().map_err(|_| "visual_state_unavailable")?;
    if state.sessions.len() >= MAX_SESSIONS && !state.sessions.contains_key(key) {
        return Err("visual_session_budget");
    }
    let session = state.sessions.entry(key.to_owned()).or_default();
    let mut value = serde_json::to_value(&session.settings).map_err(|_| "invalid_settings")?;
    let object = patch.as_object().ok_or("invalid_settings")?;
    for name in [
        "preset",
        "observations",
        "previews",
        "pointer",
        "scroll_drag",
        "keyboard",
        "opacity",
        "quality",
        "reduced_motion",
    ] {
        if let Some(v) = object.get(name) {
            value[name] = v.clone();
        }
    }
    let settings: VisualSettings = serde_json::from_value(value).map_err(|_| "invalid_settings")?;
    if !settings.validate() {
        return Err("invalid_settings");
    }
    session.settings = settings.clone();
    if !settings.enabled() {
        session.pending = None;
    }
    drop(state);
    crate::overlay::send_command(
        key.to_owned(),
        OverlayCommand::SetEnabled(settings.enabled()),
    );
    crate::overlay::send_command(
        key.to_owned(),
        OverlayCommand::SetTheme {
            theme_id: crate::overlay::current_theme_state(key)
                .map(|t| t.0)
                .unwrap_or_else(|| cursor_overlay::DEFAULT_THEME_ID.into()),
            reduced_motion: if settings.reduced_motion {
                cursor_overlay::ReducedMotion::On
            } else {
                cursor_overlay::ReducedMotion::Auto
            },
        },
    );
    wake();
    Ok(settings)
}

pub fn allows_cursor(key: &str, command: &OverlayCommand) -> bool {
    if matches!(
        command,
        OverlayCommand::SetEnabled(_)
            | OverlayCommand::SetTheme { .. }
            | OverlayCommand::SetMotion(_)
            | OverlayCommand::ApplyMotion(_)
            | OverlayCommand::SetSessionLabel(_)
            | OverlayCommand::PinAbove(_)
    ) {
        return true;
    }
    let settings = settings(key);
    if !settings.enabled() {
        return false;
    }
    match command {
        OverlayCommand::BeginAction {
            action: CursorAction::Scroll | CursorAction::Drag,
            ..
        }
        | OverlayCommand::EndAction(CursorAction::Scroll | CursorAction::Drag) => {
            settings.scroll_drag
        }
        OverlayCommand::BeginAction {
            action: CursorAction::Text | CursorAction::Key,
            ..
        }
        | OverlayCommand::EndAction(CursorAction::Text | CursorAction::Key)
        | OverlayCommand::ShowKeyCombo(_) => settings.keyboard,
        OverlayCommand::BeginAction {
            action: CursorAction::Click | CursorAction::Navigate,
            ..
        }
        | OverlayCommand::EndAction(CursorAction::Click | CursorAction::Navigate) => {
            settings.pointer
        }
        OverlayCommand::BeginAction {
            action: CursorAction::Observe,
            ..
        }
        | OverlayCommand::EndAction(CursorAction::Observe) => settings.observations,
        OverlayCommand::MoveTo { .. }
        | OverlayCommand::SnapTo { .. }
        | OverlayCommand::ClickPulse { .. }
        | OverlayCommand::SetPressed(_) => settings.pointer,
        _ => true,
    }
}

/// Admission only. Image decode, native validation and painting happen on the worker.
pub fn present(key: &str, event: ActivityEvent) -> Result<&'static str, &'static str> {
    event.validate()?;
    if !crate::overlay::draws_cursor(key) || !crate::overlay::available() {
        return Err("visual_session_unavailable");
    }
    let mut state = state().lock().map_err(|_| "visual_state_unavailable")?;
    if state.sessions.len() >= MAX_SESSIONS && !state.sessions.contains_key(key) {
        return Err("visual_session_budget");
    }
    let count = state
        .sessions
        .values()
        .filter(|s| s.pending.is_some())
        .count();
    let session = state.sessions.entry(key.to_owned()).or_default();
    if event.generation <= session.generation {
        return Ok("stale");
    }
    session.generation = event.generation;
    if event.kind == ActivityKind::Clear || !session.settings.enabled() {
        session.pending = None;
        drop(state);
        crate::overlay::clear_presentation(key);
        wake();
        return Ok("cleared");
    }
    if !session.settings.observations && event.kind != ActivityKind::Action {
        return Ok("disabled");
    }
    if count >= MAX_PENDING_SOURCES && session.pending.is_none() {
        return Err("visual_source_budget");
    }
    let mut event = event;
    if !session.settings.previews() {
        event.image = None;
    }
    session.pending = Some(event);
    drop(state);
    #[cfg(target_os = "windows")]
    {
        start()?;
        wake();
        Ok("queued")
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("visual_platform_unavailable")
    }
}

pub fn remove(key: &str) {
    if let Ok(mut state) = state().lock() {
        if let Some(session) = state.sessions.get_mut(key) {
            session.generation = u64::MAX;
            session.pending = None;
            session.settings.preset = "off".into();
        }
    }
    wake();
}
#[cfg(target_os = "windows")]
static WAKE: OnceLock<std::sync::mpsc::SyncSender<()>> = OnceLock::new();
#[cfg(target_os = "windows")]
fn start() -> Result<(), &'static str> {
    static START: OnceLock<Result<(), &'static str>> = OnceLock::new();
    *START.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        WAKE.set(tx).map_err(|_| "visual_worker_unavailable")?;
        std::thread::Builder::new()
            .name("cua-activity-hud".into())
            .spawn(move || native::run(rx))
            .map(|_| ())
            .map_err(|_| "visual_worker_unavailable")
    })
}
fn wake() {
    #[cfg(target_os = "windows")]
    if let Some(tx) = WAKE.get() {
        let _ = tx.try_send(());
    }
}
#[cfg(target_os = "windows")]
mod native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_clears_pending_without_starting_a_worker() {
        let key = "activity-off-test";
        configure(key, serde_json::json!({"preset":"off"})).unwrap();
        assert!(!settings(key).enabled());
        assert!(!allows_cursor(
            key,
            &OverlayCommand::ClickPulse { x: 1.0, y: 1.0 }
        ));
        assert!(allows_cursor(key, &OverlayCommand::SetEnabled(false)));
    }
    #[test]
    fn configuration_categories_are_independent() {
        let key = "activity-categories-test";
        configure(
            key,
            serde_json::json!({"pointer":false,"keyboard":true,"previews":false}),
        )
        .unwrap();
        assert!(!allows_cursor(
            key,
            &OverlayCommand::MoveTo {
                x: 1.0,
                y: 1.0,
                end_heading_radians: 0.0,
                target: None
            }
        ));
        assert!(allows_cursor(
            key,
            &OverlayCommand::ShowKeyCombo(vec!["ctrl".into(), "s".into()])
        ));
        assert!(!settings(key).previews());
        assert_eq!(
            configure(key, serde_json::json!({"opacity":2})).unwrap_err(),
            "invalid_settings"
        );
    }
}
