//! Closed text-free progress causes. Raw CDP/native error messages are not public reasons.

pub(super) fn progress_reason(message: &str) -> &'static str {
    match message {
        "dom_action_coverage_incomplete" => "dom_action_coverage_incomplete",
        "dom_action_ambiguous" => "dom_action_ambiguous",
        "dom_action_timeout" => "dom_action_timeout",
        "dom_action_ref_stale" => "dom_action_ref_stale",
        "dom_action_source_changed" => "dom_action_source_changed",
        "dom_action_state_unavailable" => "dom_action_state_unavailable",
        "dom_action_hit_unavailable" => "dom_action_hit_unavailable",
        "dom_action_hit_frame_unavailable" => "dom_action_hit_frame_unavailable",
        "dom_action_hit_changed" => "dom_action_hit_changed",
        "dom_action_sample_changed" => "dom_action_sample_changed",
        _ => super::dom_read_diagnostic::known_reason(message)
            .unwrap_or("dom_action_dispatch_refused"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_readiness_causes_survive_without_native_error_prose() {
        for reason in [
            "dom_action_coverage_incomplete",
            "dom_action_ambiguous",
            "dom_action_timeout",
            "dom_action_ref_stale",
            "dom_action_source_changed",
            "dom_action_state_unavailable",
            "dom_action_hit_unavailable",
            "dom_action_hit_frame_unavailable",
            "dom_action_hit_changed",
            "dom_action_sample_changed",
            "dom_read_call_function_eval_error",
            "dom_read_hit_test_exception",
            "dom_read_create_world_malformed",
        ] {
            assert_eq!(progress_reason(reason), reason);
        }
    }

    #[test]
    fn unknown_private_messages_never_become_progress_identifiers() {
        for message in [
            "private frame F_SECRET node 4932",
            "dom_action_coverage_incomplete: target private-id",
            "dom_action_arbitrary_new_reason",
            "",
        ] {
            assert_eq!(progress_reason(message), "dom_action_dispatch_refused");
        }
    }
}
