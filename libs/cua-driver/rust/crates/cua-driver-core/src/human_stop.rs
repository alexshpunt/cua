//! Human stop: the person at the machine ends all agent input from this process.
//!
//! A platform signal (on Windows, three physical Escape presses) calls [`trigger`]. From then on the
//! process refuses every tool that is not read-only, running input stops at its next step, and the
//! transport tells its host through [`subscribe`]. The stop lasts until the process exits: a host that
//! wants to continue starts a new driver once the human allows it.

use crate::protocol::ToolResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tokio::sync::watch;

/// The refusal and in-flight stop code.
pub const CODE: &str = "human_stop";

struct Stop {
    stopped: AtomicBool,
    sender: watch::Sender<bool>,
}

impl Stop {
    fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            sender: watch::channel(false).0,
        }
    }
    fn trigger(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.sender.send_replace(true);
        }
    }
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

fn global() -> &'static Stop {
    static STOP: OnceLock<Stop> = OnceLock::new();
    STOP.get_or_init(Stop::new)
}

/// Stop all agent input in this process. Idempotent; callable from any thread, including a hook.
pub fn trigger() {
    global().trigger();
}

pub fn stopped() -> bool {
    global().stopped()
}

/// The message every refusal and interrupted action carries, prefixed with [`CODE`].
pub fn message() -> String {
    format!(
        "{CODE}: the human pressed Escape three times and stopped agent input. This driver \
         accepts no more input; the host starts a new one when the human allows it."
    )
}

/// `Err` with [`message`] once the human stopped input.
pub fn check() -> Result<(), String> {
    if stopped() {
        Err(message())
    } else {
        Ok(())
    }
}

/// Holds `true` once the human stopped input; `wait_for` resolves at once when that already happened.
pub fn subscribe() -> watch::Receiver<bool> {
    global().sender.subscribe()
}

/// The refusal for a tool that is not read-only after the stop.
pub(crate) fn refusal() -> ToolResult {
    let message = message();
    ToolResult::error(message.clone()).with_structured(serde_json::json!({
        "status": "refused",
        "effect": "refused",
        "code": CODE,
        "refusal": { "code": CODE, "message": message },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trigger_latches_and_wakes_subscribers() {
        let stop = Stop::new();
        let mut receiver = stop.sender.subscribe();
        assert!(!stop.stopped());
        stop.trigger();
        stop.trigger();
        receiver.wait_for(|stopped| *stopped).await.unwrap();
        assert!(stop.stopped());
        // A late subscriber sees the stop at once.
        assert!(*stop.sender.subscribe().borrow());
    }

    #[test]
    fn refusal_names_the_code() {
        assert!(message().starts_with("human_stop:"));
        let structured = refusal().structured_content.unwrap();
        assert_eq!(structured["code"], CODE);
        assert_eq!(structured["status"], "refused");
        assert_eq!(structured["refusal"]["code"], CODE);
    }
}
