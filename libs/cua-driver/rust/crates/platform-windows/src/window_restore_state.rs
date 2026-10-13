//! Bounded, single-use exact-window state admission. The native lease owns its lifetime marker.

use std::collections::VecDeque;

/// Maximum retained unread state references across all transport sessions.
pub(crate) const MAX_STATES: usize = 64;

/// A retained native-window lifetime, independent of reusable PID/HWND values.
pub(crate) trait Lifetime {
    /// Read only: false when the original native lifetime can no longer be proven.
    fn is_current(&self) -> bool;
}

/// Exact state owner. Session identity comes from the trusted dispatch boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub pid: u32,
    pub window_id: u64,
    pub session: String,
}

/// A consumed state reference whose native lifetime remains retained through worker completion.
pub(crate) struct Admission<L> {
    pub target: Target,
    pub minimized: bool,
    /// Only a control-state Read admits management; restore-only state cannot borrow it.
    pub allow_management: bool,
    pub lifetime: L,
}

struct Entry<L> {
    id: String,
    admission: Admission<L>,
}

/// Stable refusal reasons without exposing another session's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Missing,
    TargetMismatch,
    Expired,
    NotControl,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Missing => "window_state_unavailable",
            Self::TargetMismatch => "window_state_target_mismatch",
            Self::Expired => "window_state_expired",
            Self::NotControl => "window_control_state_required",
        }
    }
}

/// Latest-per-window/session bounded cache. Removing an entry drops only its owned native lease.
pub(crate) struct Store<L> {
    entries: VecDeque<Entry<L>>,
}

impl<L: Lifetime> Default for Store<L> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<L: Lifetime> Store<L> {
    /// Publish a fresh state; retire its owner's older reference and bounded-cache victims.
    pub fn issue(&mut self, id: String, admission: Admission<L>) {
        self.entries
            .retain(|entry| entry.admission.target != admission.target);
        self.entries.push_back(Entry { id, admission });
        while self.entries.len() > MAX_STATES {
            self.entries.pop_front();
        }
    }

    /// Consume only management authority. A restore-only lease is retired on refusal.
    pub fn consume_control(&mut self, id: &str, target: &Target) -> Result<Admission<L>, Refusal> {
        let admitted = self.consume(id, target)?;
        if !admitted.allow_management {
            return Err(Refusal::NotControl);
        }
        Ok(admitted)
    }
    /// Consume a matching reference once, then check the retained native lifetime.
    /// A foreign session cannot consume or discover another session's reference.
    pub fn consume(&mut self, id: &str, target: &Target) -> Result<Admission<L>, Refusal> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id && entry.admission.target.session == target.session)
            .ok_or(Refusal::Missing)?;
        let entry = self.entries.remove(index).expect("located state exists");
        if entry.admission.target != *target {
            return Err(Refusal::TargetMismatch);
        }
        if !entry.admission.lifetime.is_current() {
            return Err(Refusal::Expired);
        }
        Ok(entry.admission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Lease {
        alive: Rc<Cell<bool>>,
        drops: Rc<Cell<usize>>,
    }
    impl Lifetime for Lease {
        fn is_current(&self) -> bool {
            self.alive.get()
        }
    }
    impl Drop for Lease {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }
    fn target(session: &str, window_id: u64) -> Target {
        Target {
            pid: 42,
            window_id,
            session: session.into(),
        }
    }
    fn lease(target: Target) -> (Admission<Lease>, Rc<Cell<bool>>, Rc<Cell<usize>>) {
        let alive = Rc::new(Cell::new(true));
        let drops = Rc::new(Cell::new(0));
        (
            Admission {
                target,
                minimized: true,
                allow_management: false,
                lifetime: Lease {
                    alive: alive.clone(),
                    drops: drops.clone(),
                },
            },
            alive,
            drops,
        )
    }

    #[test]
    fn restore_only_state_cannot_authorize_management_and_is_consumed_on_refusal() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (state, _, drops) = lease(owner.clone());
        store.issue("restore".into(), state);
        assert!(matches!(
            store.consume_control("restore", &owner),
            Err(Refusal::NotControl)
        ));
        assert_eq!(Refusal::NotControl.code(), "window_control_state_required");
        assert_eq!(drops.get(), 1);
        assert!(matches!(
            store.consume("restore", &owner),
            Err(Refusal::Missing)
        ));
    }

    #[test]
    fn control_state_uses_the_same_cache_for_management_or_existing_restore_once() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (mut control, _, _) = lease(owner.clone());
        control.allow_management = true;
        store.issue("control".into(), control);
        assert!(
            store
                .consume_control("control", &owner)
                .unwrap()
                .allow_management
        );
        assert!(matches!(
            store.consume("control", &owner),
            Err(Refusal::Missing)
        ));
        let (mut restore, _, _) = lease(owner.clone());
        restore.allow_management = true;
        store.issue("restore".into(), restore);
        assert!(store.consume("restore", &owner).unwrap().minimized);
        assert!(matches!(
            store.consume_control("restore", &owner),
            Err(Refusal::Missing)
        ));
    }
    #[test]
    fn consumed_lifetime_stays_owned_until_the_worker_drops_it() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (state, _, drops) = lease(owner.clone());
        store.issue("state-a".into(), state);
        let admitted = store.consume("state-a", &owner).unwrap();
        assert!(admitted.minimized);
        assert_eq!(drops.get(), 0);
        assert!(matches!(
            store.consume("state-a", &owner),
            Err(Refusal::Missing)
        ));
        drop(admitted);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn fresh_state_retires_only_the_same_exact_window_and_session() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (old, _, old_drops) = lease(owner.clone());
        let (other_window, _, other_drops) = lease(target("transport-a", 11));
        let (other_session, _, session_drops) = lease(target("transport-b", 10));
        store.issue("old".into(), old);
        store.issue("other-window".into(), other_window);
        store.issue("other-session".into(), other_session);
        let (fresh, _, _) = lease(owner.clone());
        store.issue("fresh".into(), fresh);
        assert_eq!(old_drops.get(), 1);
        assert_eq!(other_drops.get(), 0);
        assert_eq!(session_drops.get(), 0);
        assert!(matches!(
            store.consume("old", &owner),
            Err(Refusal::Missing)
        ));
        assert!(store.consume("fresh", &owner).is_ok());
        assert!(store
            .consume("other-window", &target("transport-a", 11))
            .is_ok());
        assert!(store
            .consume("other-session", &target("transport-b", 10))
            .is_ok());
    }

    #[test]
    fn closed_or_reused_native_lifetime_consumes_and_refuses_the_reference() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (state, alive, drops) = lease(owner.clone());
        store.issue("state".into(), state);
        alive.set(false);
        assert!(matches!(
            store.consume("state", &owner),
            Err(Refusal::Expired)
        ));
        assert_eq!(drops.get(), 1);
        assert!(matches!(
            store.consume("state", &owner),
            Err(Refusal::Missing)
        ));
    }

    #[test]
    fn session_and_exact_target_cannot_substitute_a_known_state_id() {
        let mut store = Store::default();
        let owner = target("transport-a", 10);
        let (state, _, drops) = lease(owner.clone());
        store.issue("state".into(), state);
        assert!(matches!(
            store.consume("state", &target("transport-b", 10)),
            Err(Refusal::Missing)
        ));
        assert!(matches!(
            store.consume("state", &target("transport-a", 11)),
            Err(Refusal::TargetMismatch)
        ));
        assert_eq!(drops.get(), 1);
        assert!(matches!(
            store.consume("state", &owner),
            Err(Refusal::Missing)
        ));
    }

    #[test]
    fn bounded_eviction_drops_only_the_oldest_pending_native_lease() {
        let mut store = Store::default();
        let mut drops = Vec::new();
        for index in 0..=MAX_STATES {
            let (state, _, released) = lease(target("transport-a", index as u64 + 1));
            drops.push(released);
            store.issue(format!("state-{index}"), state);
        }
        assert_eq!(drops[0].get(), 1);
        assert!(drops[1..].iter().all(|drop| drop.get() == 0));
        drop(store);
        assert!(drops.iter().all(|drop| drop.get() == 1));
    }
}
