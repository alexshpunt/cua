// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Correlates opt-in presentation with an admitted native dispatch. It grants no input authority.

use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, Weak};

const MAX_ID: u64 = 9_007_199_254_740_991;
pub const TOOL: &str = "agent_desktop_activity";

pub fn enabled() -> bool {
    cfg!(windows) && std::env::var("CUA_DESKTOP_ACTIVITY").is_ok_and(|value| value == "1")
}

pub fn take_id(args: &mut Value) -> Option<u64> {
    if !enabled() {
        return None;
    }
    args.as_object_mut()?
        .remove("activity_id")?
        .as_u64()
        .filter(|id| (1..=MAX_ID).contains(id))
}

#[derive(Clone)]
pub struct Call {
    pub owner: String,
    pub id: u64,
    pub independent_lane: bool,
    pub alive: Weak<()>,
}

tokio::task_local! {
    static CALL: Option<Call>;
}

pub async fn scope<T>(
    identity: Option<(String, u64)>,
    independent_lane: bool,
    invoke: impl Future<Output = T>,
) -> T {
    let Some((owner, id)) = identity else {
        return CALL.scope(None, invoke).await;
    };
    let alive = Arc::new(());
    let context = Call {
        owner,
        id,
        independent_lane,
        alive: Arc::downgrade(&alive),
    };
    let result = CALL.scope(Some(context), invoke).await;
    drop(alive);
    result
}

pub fn current() -> Option<Call> {
    CALL.try_with(Clone::clone).ok().flatten()
}

pub fn spawn_blocking<F, R>(operation: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let call = current();
    let admission = crate::tool::retain_desktop_action_admission();
    tokio::task::spawn_blocking(move || {
        crate::tool::with_action_admission(admission, || CALL.sync_scope(call, operation))
    })
}
