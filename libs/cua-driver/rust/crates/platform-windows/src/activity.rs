//! Session-owned human-only presentation. The worker has no input/capture operations.

use cursor_overlay::activity::{ActivityEvent, ActivityKind, VisualSettings};
use cursor_overlay::{CursorAction, OverlayCommand};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

// The worker carries one ticket from admission through its final commit.
struct Work {
    event: ActivityEvent,
    admitted: Instant,
    epoch: u64,
}

impl Work {
    fn ready(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.admitted).as_millis() < 250
    }
}
const MAX_SESSIONS: usize = 64;
const MAX_PENDING_SOURCES: usize = 4;
#[derive(Default)]
struct Session {
    settings: VisualSettings,
    generation: u64,
    epoch: u64,
    commit: Arc<Mutex<()>>,
    pending: Option<Work>,
    ended: bool,
}
impl Session {
    fn current(&self, work: &Work, now: Instant) -> bool {
        !self.ended
            && self.generation == work.event.generation
            && self.epoch == work.epoch
            && now.saturating_duration_since(work.admitted).as_millis()
                < u128::from(cursor_overlay::activity::TTL_MS)
            && event_allowed(&self.settings, &work.event)
    }
    fn set_settings(&mut self, settings: VisualSettings) {
        self.epoch = self.epoch.wrapping_add(1);
        self.settings = settings;
        self.pending = None;
    }
    fn advance(&mut self, generation: u64) -> bool {
        if self.ended || generation <= self.generation {
            return false;
        }
        self.generation = generation;
        true
    }
    fn end(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
        self.generation = self.generation.saturating_add(1);
        self.ended = true;
    }
    fn revive(&mut self) {
        self.ended = false;
    }
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn session_gate(key: &str) -> Result<Arc<Mutex<()>>, &'static str> {
    let mut state = state().lock().map_err(|_| "visual_state_unavailable")?;
    if state.sessions.len() >= MAX_SESSIONS && !state.sessions.contains_key(key) {
        return Err("visual_session_budget");
    }
    Ok(state
        .sessions
        .entry(key.to_owned())
        .or_default()
        .commit
        .clone())
}

fn event_allowed(settings: &VisualSettings, event: &ActivityEvent) -> bool {
    settings.enabled()
        && if event.kind == ActivityKind::Action {
            match event.action.as_deref() {
                Some("type_text" | "type" | "fill" | "press_key" | "hotkey" | "keypress") => {
                    settings.keyboard
                }
                Some("scroll" | "scroll_at" | "drag" | "input_timeline") => settings.scroll_drag,
                _ => settings.pointer,
            }
        } else {
            settings.observations
        }
}

fn with_current<T>(key: &str, work: &Work, commit: impl FnOnce(VisualSettings) -> T) -> Option<T> {
    let gate = state().lock().ok()?.sessions.get(key)?.commit.clone();
    let _commit = gate.lock().ok()?;
    // Never retain STATE while painting or enqueueing: input only reads that short-lived lock.
    let settings = {
        let state = state().lock().ok()?;
        let session = state.sessions.get(key)?;
        session
            .current(work, Instant::now())
            .then(|| session.settings.clone())?
    };
    Some(commit(settings))
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
    let gate = session_gate(key)?;
    let _commit = gate.lock().map_err(|_| "visual_state_unavailable")?;
    let mut state = state().lock().map_err(|_| "visual_state_unavailable")?;
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
    session.set_settings(settings.clone());
    drop(state);
    let cleared = clear_native(key);
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
    cleared?;
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
        // Native drag tracking uses immediate positions and a balanced visual press.
        OverlayCommand::SnapTo { .. } | OverlayCommand::SetPressed(_) => settings.scroll_drag,
        OverlayCommand::MoveTo { .. } | OverlayCommand::ClickPulse { .. } => settings.pointer,
        _ => true,
    }
}

/// Admission only. Image decode, native validation and painting happen on the worker.
pub fn present(key: &str, event: ActivityEvent) -> Result<&'static str, &'static str> {
    let admitted = Instant::now();
    event.validate()?;
    if !crate::overlay::draws_cursor(key) || !crate::overlay::available() {
        return Err("visual_session_unavailable");
    }
    let gate = session_gate(key)?;
    let _commit = gate.lock().map_err(|_| "visual_state_unavailable")?;
    let mut state = state().lock().map_err(|_| "visual_state_unavailable")?;
    let count = state
        .sessions
        .values()
        .filter(|s| s.pending.is_some())
        .count();
    let session = state.sessions.entry(key.to_owned()).or_default();
    if session.ended {
        return Err("visual_session_ended");
    }
    if !session.advance(event.generation) {
        return Ok("stale");
    }
    if event.kind == ActivityKind::Clear || !session.settings.enabled() {
        session.pending = None;
        drop(state);
        clear_native(key)?;
        wake();
        return Ok("cleared");
    }
    if !event_allowed(&session.settings, &event) {
        return Ok("disabled");
    }
    if count >= MAX_PENDING_SOURCES && session.pending.is_none() {
        return Err("visual_source_budget");
    }
    let mut event = event;
    if !session.settings.previews() {
        event.image = None;
    }
    session.pending = Some(Work {
        event,
        admitted,
        epoch: session.epoch,
    });
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

/// Called with the session commit gate held, after state invalidation.
fn clear_native(key: &str) -> Result<(), &'static str> {
    crate::overlay::clear_presentation(key);
    #[cfg(target_os = "windows")]
    native::hide(key)?;
    Ok(())
}
pub fn remove(key: &str) {
    let Some(gate) = state()
        .lock()
        .ok()
        .and_then(|s| s.sessions.get(key).map(|s| s.commit.clone()))
    else {
        return;
    };
    let Ok(_commit) = gate.lock() else { return };
    if let Ok(mut state) = state().lock() {
        if let Some(session) = state.sessions.get_mut(key) {
            session.end();
        }
    }
    if let Err(reason) = clear_native(key) {
        tracing::warn!(reason, "Native HUD cleanup was unavailable");
    }
    wake();
}
pub fn revive(key: &str) {
    let Some(gate) = state()
        .lock()
        .ok()
        .and_then(|s| s.sessions.get(key).map(|s| s.commit.clone()))
    else {
        return;
    };
    let Ok(_commit) = gate.lock() else { return };
    if let Ok(mut state) = state().lock() {
        if let Some(session) = state.sessions.get_mut(key) {
            session.revive();
        }
    }
}
/// The native operation owns completion; presentation has no delivery authority.
pub(crate) async fn alongside_presentation<T>(
    operation: impl std::future::Future<Output = T>,
    presentation: impl std::future::Future<Output = ()>,
) -> T {
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        () = presentation => operation.await,
    }
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
    fn generations_reject_late_events_and_survive_session_revival() {
        let mut session = Session::default();
        assert!(session.advance(7));
        assert!(!session.advance(7));
        assert!(!session.advance(6));
        session.end();
        assert!(!session.advance(9));
        session.revive();
        assert!(!session.advance(8));
        assert!(session.advance(9));
        assert!(session.settings.enabled());
    }
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
