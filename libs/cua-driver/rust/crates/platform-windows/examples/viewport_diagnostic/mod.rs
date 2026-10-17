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

#[cfg(test)]
mod tests {
    use super::*;

    fn window(visible: bool, class_name: Option<&str>) -> WindowRow<()> {
        WindowRow {
            visible,
            class_name: class_name.map(str::to_owned),
            data: (),
        }
    }

    #[test]
    fn hidden_legacy_windows_are_retained_and_make_visible_match_ambiguous() {
        let mut scan = Scan::new(3);
        for visible in [false, true] {
            assert!(scan.admit());
            scan.record(window(visible, Some("Chrome_RenderWidgetHostHWND")));
        }
        assert!(!scan.rows[0].visible);
        assert_eq!(legacy_candidates(&scan), (2, "multiple_candidates"));
    }

    #[test]
    fn unknown_classes_cannot_be_silently_excluded_from_uniqueness() {
        let mut scan = Scan::new(3);
        scan.record(window(true, Some("Chrome_RenderWidgetHostHWND")));
        scan.record(window(false, None));
        assert_eq!(legacy_candidates(&scan), (1, "unknown"));
    }

    #[test]
    fn exhaustion_preserves_prefix_but_cannot_prove_a_unique_candidate() {
        let mut scan = Scan::new(1);
        assert!(scan.admit());
        scan.record(window(false, Some("Chrome_RenderWidgetHostHWND")));
        assert!(!scan.admit());
        assert!(!scan.admit());
        assert_eq!(scan.rows.len(), 1);
        assert_eq!(scan.stop, Some("node_budget"));
        assert_eq!(legacy_candidates(&scan), (1, "unknown"));
    }

    #[test]
    fn exact_budget_is_complete_only_when_no_more_nodes_are_discovered() {
        let mut scan = Scan::new(1);
        assert!(scan.admit());
        scan.record(window(true, Some("Other")));
        assert_eq!(legacy_candidates(&scan), (0, "none"));
        assert!(!scan.admit());
        assert_eq!(legacy_candidates(&scan), (0, "unknown"));
    }

    #[test]
    fn document_count_never_becomes_a_first_match_or_a_mapping() {
        assert_eq!(candidate_state(0, true), "none");
        assert_eq!(candidate_state(1, true), "one_candidate_not_qualified");
        assert_eq!(candidate_state(2, true), "multiple_candidates");
        for count in [0, 1, 2] {
            assert_eq!(candidate_state(count, false), "unknown");
        }
    }
}
