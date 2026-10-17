//! Bounded collection policy for the readonly native viewport diagnostic.
//! A candidate count is diagnostic information, never a selected viewport.

#[cfg(target_os = "windows")]
pub mod native;

pub const HWND_LIMIT: usize = 1024;
pub const UIA_LIMIT: usize = 512;
pub const UIA_DEPTH_LIMIT: usize = 24;
pub const DEADLINE_MS: u64 = 5000;

/// Native metadata is retained even for hidden windows or unavailable classes.
pub struct WindowRow<T> {
    pub visible: bool,
    pub class_name: Option<String>,
    pub data: T,
}

/// Stop before retaining more than the fixed budget; a stopped scan is incomplete.
pub struct Scan<T> {
    pub rows: Vec<T>,
    pub stop: Option<&'static str>,
    limit: usize,
}

impl<T> Scan<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            rows: Vec::new(),
            stop: None,
            limit,
        }
    }

    pub fn admit(&mut self) -> bool {
        if self.stop.is_some() {
            return false;
        }
        if self.rows.len() == self.limit {
            self.stop = Some("node_budget");
            return false;
        }
        true
    }

    pub fn record(&mut self, row: T) {
        assert!(self.rows.len() < self.limit && self.stop.is_none());
        self.rows.push(row);
    }
}

/// Unknown/incomplete discovery cannot imply either absence or uniqueness.
pub fn candidate_state(count: usize, complete: bool) -> &'static str {
    if !complete {
        "unknown"
    } else {
        match count {
            0 => "none",
            1 => "one_candidate_not_qualified",
            _ => "multiple_candidates",
        }
    }
}

/// Count every legacy renderer HWND, including hidden ones; do not pick one.
pub fn legacy_candidates<T>(scan: &Scan<WindowRow<T>>) -> (usize, &'static str) {
    let count = scan
        .rows
        .iter()
        .filter(|row| row.class_name.as_deref() == Some("Chrome_RenderWidgetHostHWND"))
        .count();
    let complete = scan.stop.is_none() && scan.rows.iter().all(|row| row.class_name.is_some());
    (count, candidate_state(count, complete))
}

