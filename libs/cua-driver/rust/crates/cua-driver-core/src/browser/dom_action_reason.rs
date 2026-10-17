//! Closed text-free progress causes. Raw CDP/native error messages are not public reasons.

pub(super) fn progress_reason(message: &str) -> &'static str {
    match message {
        "dom_action_coverage_incomplete" => "dom_action_coverage_incomplete",
        "dom_action_ambiguous" => "dom_action_ambiguous",
        "dom_action_timeout" => "dom_action_timeout",
        "dom_action_ref_stale" => "dom_action_ref_stale",
        "dom_action_source_changed" => "dom_action_source_changed",
        _ => "dom_action_dispatch_refused",
    }
}
