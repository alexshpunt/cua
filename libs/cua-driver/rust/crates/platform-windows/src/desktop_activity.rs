// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Bounded presentation state. No input argument bodies or window titles enter it.

use cua_driver_contract::{
    DesktopActivityDelivery, DesktopActivityInput as Update, DesktopActivityKind,
    DesktopActivityLocation, DesktopActivityOutcome, DesktopActivityWindow as Window,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Weak;

#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod native;
#[cfg(windows)]
pub(crate) use self::ipc::{apply, end, note_direct, note_pointer_arrival, note_shared};

const LIMIT: usize = 8;
const RECENT_MS: u64 = 20_000;
const STALE_MS: u64 = 2_000;

macro_rules! labels {
    ($name:ident from $input:ident {$($variant:ident => $label:literal),+ $(,)?}
        $(only {$($internal:ident => $internal_label:literal),+ $(,)?})?
    ) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(super) enum $name {$($variant),+ $($(, $internal)+)?}
        impl $name {
            pub(super) fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                    $($(Self::$internal => $internal_label,)+)?
                }
            }
        }
        impl From<$input> for $name {
            fn from(value: $input) -> Self {
                match value { $($input::$variant => Self::$variant,)+ }
            }
        }
    };
}
labels!(Kind from DesktopActivityKind {
    Read => "Reading", Wait => "Waiting", Click => "Clicking", Drag => "Dragging",
    Scroll => "Scrolling", MovePointer => "Moving pointer", Type => "Typing", Key => "Pressing keys",
    Open => "Opening", Manage => "Managing window", Browser => "Using browser", Clipboard => "Using clipboard",
    Files => "Working with files", Record => "Recording", Other => "Working",
} only { SessionStarted => "Session started", SessionEnded => "Session ended" });
labels!(Location from DesktopActivityLocation { Desktop => "Desktop", Browser => "Browser", SeparateDesktop => "Separate desktop" });
labels!(Delivery from DesktopActivityDelivery { None => "No input", Direct => "Background", Unknown => "Input route unknown" }
    only { Shared => "Shared input" });
labels!(Outcome from DesktopActivityOutcome { Completed => "Completed", Refused => "Refused", Interrupted => "Interrupted", Unknown => "Unknown" });

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Action {
    pub(super) kind: Kind,
    pub(super) location: Location,
    pub(super) delivery: Delivery,
    pub(super) window: Option<Window>,
    pub(super) position: Option<(i32, i32)>,
    pub(super) position_frame: Option<cursor_overlay::activity::ActivityRect>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Recent {
    pub(super) kind: Kind,
    pub(super) outcome: Outcome,
    pub(super) at_ms: u64,
    pub(super) shared: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Agent {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) intent: String,
    pub(super) active: bool,
    pub(super) action: Option<Action>,
    pub(super) recent: Vec<Recent>,
    pub(super) panel: bool,
    pub(super) outline: bool,
    pub(super) point: bool,
}

struct Pending {
    action: Action,
    order: u64,
    shared: Option<Weak<()>>,
}

impl Pending {
    fn sharing_now(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|call| call.strong_count() > 0)
    }
}

pub(super) struct Session {
    row: Agent,
    pending: BTreeMap<u64, Pending>,
    order: u64,
}

impl Session {
    fn new(id: String) -> Self {
        Self {
            row: Agent {
                id,
                label: String::new(),
                intent: String::new(),
                active: false,
                action: None,
                recent: Vec::new(),
                panel: false,
                outline: false,
                point: false,
            },
            pending: BTreeMap::new(),
            order: 0,
        }
    }

    fn add_recent(&mut self, kind: Kind, outcome: Outcome, at_ms: u64, shared: bool) {
        self.row.recent.insert(
            0,
            Recent {
                kind,
                outcome,
                at_ms,
                shared,
            },
        );
        self.row.recent.truncate(LIMIT);
    }

    fn finish(&mut self, outcome: Outcome, now: u64) -> bool {
        if !self.row.active {
            return false;
        }
        for entry in std::mem::take(&mut self.pending).into_values() {
            self.add_recent(
                entry.action.kind,
                Outcome::Interrupted,
                now,
                entry.shared.is_some(),
            );
        }
        self.row.active = false;
        self.row.intent.clear();
        self.add_recent(Kind::SessionEnded, outcome, now, false);
        true
    }

    fn apply(&mut self, update: Update, now: u64) -> Result<bool, &'static str> {
        update.validate_fields()?;
        match update {
            Update::Configure {
                label,
                panel,
                outline,
                point,
            } => {
                self.row.label = label.trim().to_owned();
                self.row.panel = panel;
                self.row.outline = outline;
                self.row.point = point;
                if !point {
                    for pending in self.pending.values_mut() {
                        pending.action.position = None;
                        pending.action.position_frame = None;
                    }
                }
            }
            Update::Begin { intent } => {
                if self.row.label.is_empty() {
                    return Err("activity_unconfigured");
                }
                if self.row.active {
                    return Err("activity_already_active");
                }
                self.row.active = true;
                self.row.intent = intent.trim().to_owned();
                self.add_recent(Kind::SessionStarted, Outcome::Completed, now, false);
            }
            Update::End { outcome } => {
                return Ok(self.finish(outcome.into(), now));
            }
            Update::ActionStart {
                id,
                kind,
                location,
                window,
                delivery,
            } => {
                if !self.row.active || self.pending.contains_key(&id) {
                    return Ok(false);
                }
                if self.pending.len() >= LIMIT {
                    return Err("activity_busy");
                }
                if window.as_ref().is_some_and(|window| {
                    cursor_overlay::activity::identity_property(&window.identity, window.pid)
                        .is_none()
                }) {
                    return Err("invalid_activity_window");
                }
                self.order += 1;
                self.pending.insert(
                    id,
                    Pending {
                        action: Action {
                            kind: kind.into(),
                            location: location.into(),
                            delivery: delivery.map_or(Delivery::Unknown, Delivery::from),
                            window,
                            position: None,
                            position_frame: None,
                        },
                        order: self.order,
                        shared: None,
                    },
                );
            }
            Update::ActionEnd { id, outcome } => {
                let Some(entry) = self.pending.remove(&id) else {
                    return Ok(false);
                };
                self.add_recent(
                    entry.action.kind,
                    outcome.into(),
                    now,
                    entry.shared.is_some(),
                );
            }
        }
        Ok(true)
    }

    // A point comes from a validated native edge, never from the publication request.
    fn note(
        &mut self,
        call: &cua_driver_core::desktop_activity::Call,
        shared: bool,
        position: Option<(i32, i32)>,
    ) {
        if call.alive.strong_count() == 0 {
            return;
        }
        let Some(entry) = self.pending.get_mut(&call.id) else {
            return;
        };
        if entry.action.location != Location::Desktop {
            return;
        }
        if shared {
            entry.shared = Some(call.alive.clone());
        } else if call.independent_lane && entry.action.delivery == Delivery::Unknown {
            entry.action.delivery = Delivery::Direct;
        }
        if position.is_some() && self.row.point {
            entry.action.position = position;
            #[cfg(windows)]
            {
                entry.action.position_frame = entry.action.window.as_ref().and_then(|w| {
                    crate::activity::native::exact_window_frame(w.window_id, w.pid, &w.identity)
                });
            }
        }
    }

    fn project(&mut self, now: u64) -> Agent {
        self.row
            .recent
            .retain(|entry| now.saturating_sub(entry.at_ms) < RECENT_MS);
        let mut row = self.row.clone();
        if let Some(entry) = self
            .pending
            .values()
            .max_by_key(|entry| (entry.sharing_now(), entry.order))
        {
            let mut action = entry.action.clone();
            if entry.sharing_now() {
                action.delivery = Delivery::Shared;
            }
            if !row.point {
                action.position = None;
            }
            if !row.outline && !row.panel && !row.point {
                action.window = None;
            }
            row.action = Some(action);
        }
        row
    }
}

/// A vanished publisher keeps recent entries but cannot keep an active row or cues.
fn expire(row: &mut Agent, seen: u64, now: u64) {
    let age = now.saturating_sub(seen);
    if age >= STALE_MS && row.active {
        row.intent.clear();
        row.active = false;
        row.action = None;
        row.outline = false;
        row.point = false;
        row.panel = false;
        row.recent.insert(
            0,
            Recent {
                kind: Kind::SessionEnded,
                outcome: Outcome::Interrupted,
                at_ms: seen.saturating_add(STALE_MS),
                shared: false,
            },
        );
    }
    row.recent
        .retain(|entry| now.saturating_sub(entry.at_ms) < RECENT_MS);
    row.recent.truncate(LIMIT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn active() -> Session {
        let mut session = Session::new("publisher".into());
        assert_eq!(
            session.apply(
                Update::Configure {
                    label: "Agent".into(),
                    panel: true,
                    outline: true,
                    point: true
                },
                0
            ),
            Ok(true)
        );
        assert_eq!(
            session.apply(
                Update::Begin {
                    intent: "Check the app".into()
                },
                1
            ),
            Ok(true)
        );
        session
    }

    fn start(id: u64, location: DesktopActivityLocation) -> Update {
        Update::ActionStart {
            id,
            kind: DesktopActivityKind::Click,
            location,
            window: None,
            delivery: None,
        }
    }

    #[test]
    fn concurrent_browser_action_does_not_hide_live_native_input_and_late_completion_is_ignored() {
        let mut session = active();
        assert_eq!(
            session.apply(start(2, DesktopActivityLocation::Desktop), 2),
            Ok(true)
        );
        assert_eq!(
            session.apply(start(1, DesktopActivityLocation::Browser), 3),
            Ok(true)
        );
        let alive = Arc::new(());
        let call = cua_driver_core::desktop_activity::Call {
            owner: String::new(),
            id: 2,
            independent_lane: false,
            alive: Arc::downgrade(&alive),
        };
        session.note(&call, true, Some((-10, 15)));
        let shown = session.project(4).action.unwrap();
        assert_eq!(shown.location, Location::Desktop);
        assert_eq!(shown.delivery, Delivery::Shared);
        assert_eq!(shown.position, Some((-10, 15)));
        drop(alive);
        let shown = session.project(5).action.unwrap();
        assert_eq!(shown.location, Location::Browser);
        assert_eq!(shown.delivery, Delivery::Unknown);
        assert_eq!(
            session.apply(
                Update::ActionEnd {
                    id: 2,
                    outcome: DesktopActivityOutcome::Completed
                },
                6
            ),
            Ok(true)
        );
        assert!(session.project(6).recent[0].shared);
        assert_eq!(
            session.apply(
                Update::End {
                    outcome: DesktopActivityOutcome::Interrupted
                },
                7
            ),
            Ok(true)
        );
        assert_eq!(
            session.apply(
                Update::ActionEnd {
                    id: 1,
                    outcome: DesktopActivityOutcome::Completed
                },
                8
            ),
            Ok(false)
        );
        let shown = session.project(8);
        assert!(!shown.active);
        assert!(shown.action.is_none());
        assert_eq!(shown.recent[0].outcome, Outcome::Interrupted);
    }

    #[test]
    fn stale_publisher_expires_without_keepalive_and_history_is_bounded() {
        let mut session = active();
        for id in 1..25 {
            assert_eq!(
                session.apply(start(id, DesktopActivityLocation::Desktop), id),
                Ok(true)
            );
            assert_eq!(
                session.apply(
                    Update::ActionEnd {
                        id,
                        outcome: DesktopActivityOutcome::Completed
                    },
                    id
                ),
                Ok(true)
            );
        }
        assert_eq!(
            session.apply(start(25, DesktopActivityLocation::Desktop), 25),
            Ok(true)
        );
        let mut row = session.project(25);
        expire(&mut row, 25, 25 + STALE_MS - 1);
        assert!(row.active);
        expire(&mut row, 25, 25 + STALE_MS);
        assert!(!row.active);
        assert!(row.action.is_none());
        assert!(!row.panel && !row.outline && !row.point);
        assert_eq!(row.recent.len(), LIMIT);
        assert_eq!(row.recent[0].outcome, Outcome::Interrupted);
        expire(&mut row, 25, 25 + STALE_MS + RECENT_MS);
        assert!(row.recent.is_empty());
    }
}
