// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

//! When one foreground input turn may give the user's window back.
//!
//! Input the target thread has not read yet is read by whichever window is
//! foreground at that moment (#4477), so restoring the previous window early
//! hands it the unread tail. A late keystroke in the user's window cannot be
//! undone; a target left in front costs one click. The confirmed keyboard subset
//! is modelled in `.cursor/skills/formally-verify-cua-driver/models/windows_typing.tla`.
//! Unobservable streams stay with the target; no later unrelated read acknowledges
//! their earlier prefix.
//! Pure, so the table test runs on every host.

pub(crate) use cua_driver_core::protocol::InputReadCompletion as TypingTurn;

/// Whether to give the previous foreground back. Never while inserted input
/// may be unread, and never from a window that took the foreground itself.
pub(crate) fn restores_previous(turn: TypingTurn) -> bool {
    match turn {
        TypingTurn::NotSent | TypingTurn::Drained => true,
        TypingTurn::Unobservable | TypingTurn::Undrained | TypingTurn::Lost => false,
    }
}

/// Carry read knowledge to the focus lease without changing public structured
/// output. Uncertainty stays in text and the action's existing effect contract.
pub(crate) fn completion_result(
    mut result: cua_driver_core::protocol::ToolResult,
    turn: TypingTurn,
) -> cua_driver_core::protocol::ToolResult {
    result.input_read = Some(turn);
    if turn == TypingTurn::Unobservable {
        result
            .content
            .push(cua_driver_core::protocol::Content::text(
                "input_read: unobservable; no supported witness confirms that the target read \
             every inserted event. Automatic foreground return is suppressed. Nothing was \
             replayed; inspect the target before repeating the action.",
            ));
    }
    result
}

/// Read knowledge retained on failures, including a confirmed earlier prefix.
#[derive(Debug, thiserror::Error)]
#[error("{error}{}", restoration_note(*.turn))]
pub(crate) struct ForegroundInputError {
    pub(crate) error: anyhow::Error,
    pub(crate) turn: TypingTurn,
}

fn restoration_note(turn: TypingTurn) -> String {
    if restores_previous(turn) {
        String::new()
    } else {
        format!(" Focus was not restored: {}", reason(turn))
    }
}
fn reason(turn: TypingTurn) -> &'static str {
    if turn == TypingTurn::Lost {
        "another window took the foreground during the call, and the driver does not take \
         it back."
    } else {
        "the target window stays in front so input it has not read yet cannot reach the \
         previously foreground window. Bring that window back once the target settles."
    }
}

/// The tool result for a failed foreground input call.
pub(crate) fn foreground_input_error(
    error: anyhow::Error,
) -> cua_driver_core::protocol::ToolResult {
    let turn = error
        .downcast_ref::<ForegroundInputError>()
        .map(|error| error.turn)
        .unwrap_or(TypingTurn::NotSent);
    let result = cua_driver_core::protocol::ToolResult::error(error.to_string());
    let result = if restores_previous(turn) {
        result
    } else {
        result.with_structured(serde_json::json!({ "foreground_restored": false }))
    };
    completion_result(result, turn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use TypingTurn::*;

    const RESTORE: bool = true;
    const LEAVE: bool = false;

    /// One row per exit of `with_confirmed_foreground` once the target is
    /// confirmed in front: the turn the exit produces and the expected outcome.
    #[test]
    fn each_exit_restores_or_leaves_the_target_in_front() {
        #[rustfmt::skip]
        let rows: &[(&str, TypingTurn, bool)] = &[
            ("focus step or owned-window check fails", NotSent,   RESTORE),
            ("SendInput inserted nothing",             NotSent,   RESTORE),
            ("foreground_lost before a text unit",     Lost,      LEAVE),
            ("text unit SendInput short",              Undrained, LEAVE),
            ("text_delivery_stalled",                  Undrained, LEAVE),
            ("key chord SendInput short",              Undrained, LEAVE),
            ("unpaced text SendInput short",           Undrained, LEAVE),
            ("final drain timed out",                  Undrained, LEAVE),
            ("every text unit read",                   Drained,   RESTORE),
            ("every requested key pulse read",         Drained,   RESTORE),
            ("no supported receiver-read witness",     Unobservable, LEAVE),
        ];
        for &(exit, turn, expected) in rows {
            assert_eq!(restores_previous(turn), expected, "{exit}");
        }
    }

    #[test]
    fn a_later_read_cannot_erase_an_earlier_unconfirmed_prefix() {
        for earlier in [Unobservable, Undrained, Lost] {
            assert_eq!(earlier.combine(Drained), earlier);
            assert_eq!(earlier.combine(NotSent), earlier);
            assert_eq!(Drained.combine(earlier), earlier);
        }
        assert_eq!(NotSent.combine(Drained), Drained);
    }
    #[test]
    fn an_unrestored_error_retains_its_read_completion_and_public_flag() {
        let stalled = anyhow::anyhow!("text_delivery_stalled: unit 3 of 5 was not read.");
        let result = foreground_input_error(
            ForegroundInputError {
                error: stalled,
                turn: Undrained,
            }
            .into(),
        );
        let text = serde_json::to_value(&result.content).unwrap().to_string();
        assert!(
            text.contains("text_delivery_stalled: unit 3 of 5"),
            "{text}"
        );
        assert_eq!(result.input_read, Some(Undrained));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "foreground_restored": false }))
        );

        let refused = foreground_input_error(anyhow::anyhow!("foreground_unavailable: no input"));
        assert_eq!(refused.is_error, Some(true));
        assert_eq!(refused.input_read, Some(NotSent));
        assert_eq!(refused.structured_content, None);
    }
}
