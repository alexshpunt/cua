//! Bounded presentation playback. These deadlines never govern actual input.

use std::collections::VecDeque;

pub const MAX_PENDING: usize = 32;
pub const MAX_LAG_MS: u64 = 500;

struct Entry<T> {
    at: u64,
    value: T,
    dwell_ms: u64,
    navigation: bool,
}

pub struct Playback<T> {
    pending: VecDeque<Entry<T>>,
    until: Option<u64>,
}
impl<T> Default for Playback<T> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            until: None,
        }
    }
}
impl<T> Playback<T> {
    /// Admission is constant-time and cannot wait for a render frame.
    pub fn push(&mut self, now: u64, value: T, dwell_ms: u64, navigation: bool) {
        if navigation && self.pending.back().is_some_and(|item| item.navigation) {
            self.pending.pop_back();
        }
        while self.pending.len() >= MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back(Entry {
            at: now,
            value,
            dwell_ms,
            navigation,
        });
    }
    pub fn next(&mut self, now: u64) -> Option<T> {
        while self
            .pending
            .front()
            .is_some_and(|item| now.saturating_sub(item.at) > MAX_LAG_MS)
        {
            self.pending.pop_front();
        }
        if self.until.is_some_and(|until| now < until) {
            return None;
        }
        self.until = None;
        let item = self.pending.pop_front()?;
        let backlog = (self.pending.len() as u64 / 4 + 1).min(8);
        self.until = Some(now.saturating_add(item.dwell_ms / backlog));
        Some(item.value)
    }
    pub fn remaining(&self, now: u64) -> u64 {
        self.until.map_or(0, |until| until.saturating_sub(now))
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.until = None;
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.until.is_none()
    }
    pub fn len(&self) -> usize {
        self.pending.len()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_move_then_click_without_waiting_in_the_producer() {
        let mut queue = Playback::default();
        queue.push(10, 1, 180, false);
        queue.push(10, 2, 70, false);
        assert_eq!(queue.next(10), Some(1));
        assert_eq!(queue.next(189), None);
        assert_eq!(queue.next(190), Some(2));
        assert!(!queue.is_empty());
        assert_eq!(queue.next(260), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn consecutive_navigation_coalesces_but_clicks_keep_their_order() {
        let mut queue = Playback::default();
        queue.push(0, 1, 180, true);
        queue.push(2, 2, 180, true);
        queue.push(3, 3, 70, false);
        queue.push(4, 4, 180, true);
        assert_eq!(queue.next(4), Some(2));
        assert_eq!(queue.next(184), Some(3));
        assert_eq!(queue.next(254), Some(4));
    }

    #[test]
    fn flood_is_bounded_and_does_not_replay_seconds_of_history() {
        let mut queue = Playback::default();
        for n in 0..1000 {
            queue.push(n, n, 180, false);
        }
        assert!(queue.len() <= MAX_PENDING);
        assert!(queue.next(2000).is_none());
        assert!(queue.is_empty());
        queue.push(2001, 1001, 0, false);
        assert_eq!(queue.next(2001), Some(1001));
    }

    #[test]
    fn clear_invalidates_every_queued_effect_and_its_deadline() {
        let mut queue = Playback::default();
        queue.push(0, 1, 180, false);
        assert_eq!(queue.next(0), Some(1));
        queue.push(1, 2, 80, false);
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.next(2), None);
    }
}
