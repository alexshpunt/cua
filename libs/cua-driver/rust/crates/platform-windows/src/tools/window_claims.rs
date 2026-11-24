// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Window claims: an agent takes a window so that other agents' driver processes on the same desktop
//! refuse input into it. The claims themselves live in [`cua_driver_core::window_claims`].

use async_trait::async_trait;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{ProtectedResourceOwnership, ProtectedResourceScopeError, Tool, ToolDef},
    window_claims,
};
use serde_json::{json, Value};

/// The longest holder name shown to other agents.
const HOLDER_MAX_CHARS: usize = 80;

/// A holder name without control characters, trimmed and bounded.
fn holder_name(value: Option<&str>) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter(|character| !character.is_control())
        .take(HOLDER_MAX_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// The refusal for input into a window another agent holds.
pub(crate) fn claimed_refusal(window_id: u64, holder: &str) -> ToolResult {
    let message = window_claims::refusal_message(window_id, holder);
    ToolResult::error(message.clone()).with_structured(json!({
        "status": "refused",
        "effect": "refused",
        "code": window_claims::CLAIMED_CODE,
        "window_id": window_id,
        "holder": holder,
        "refusal": { "code": window_claims::CLAIMED_CODE, "message": message },
    }))
}

/// The exact window a call names, by its top-level `window_id` or its `target`.
fn named_window(args: &Value) -> Option<u64> {
    args.get("window_id")
        .and_then(Value::as_u64)
        .or_else(|| args.get("target")?.get("window_id")?.as_u64())
        .filter(|window_id| *window_id > 0)
}

/// The agent of another driver process that holds the window a call names, if any.
async fn other_holder(args: &Value) -> Option<(u64, String)> {
    let window_id = named_window(args)?;
    tokio::task::spawn_blocking(move || {
        let owner_pid = crate::win32::window_owner_pid(window_id)?;
        window_claims::held_by_other(window_id, owner_pid).map(|holder| (window_id, holder))
    })
    .await
    .ok()
    .flatten()
}

/// Refuses a call that changes a window another agent holds; read-only calls pass.
pub(crate) struct WindowClaimGuard {
    pub(crate) inner: Box<dyn Tool>,
}

impl WindowClaimGuard {
    pub(crate) fn boxed(tool: impl Tool + 'static) -> Box<dyn Tool> {
        Box::new(Self {
            inner: Box::new(tool),
        })
    }
}

#[async_trait]
impl Tool for WindowClaimGuard {
    fn def(&self) -> &ToolDef {
        self.inner.def()
    }

    fn has_independent_input_lane(&self, args: &Value) -> bool {
        self.inner.has_independent_input_lane(args)
    }

    async fn protected_resource_ownership(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> ProtectedResourceOwnership {
        self.inner
            .protected_resource_ownership(adapter_id, args)
            .await
    }

    async fn protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, ProtectedResourceScopeError> {
        self.inner.protected_resource_scope(adapter_id, args).await
    }

    async fn validate_protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
        approved_scope: &Value,
    ) -> Result<(), ProtectedResourceScopeError> {
        self.inner
            .validate_protected_resource_scope(adapter_id, args, approved_scope)
            .await
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        if !self.inner.def().read_only {
            if let Some((window_id, holder)) = other_holder(&args).await {
                return claimed_refusal(window_id, &holder);
            }
        }
        self.inner.invoke(args).await
    }
}

// ── claim_window ──────────────────────────────────────────────────────────────

pub struct ClaimWindowTool;
static CLAIM_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for ClaimWindowTool {
    fn def(&self) -> &ToolDef {
        CLAIM_DEF.get_or_init(|| ToolDef {
            name: "claim_window".into(),
            description: "Hold an exact window for this driver process: while it holds the window, \
                other Cua Driver processes on this desktop refuse input and window changes there \
                with code window_claimed and the holder's name. The hold lasts until \
                release_window or the end of this process. A window another process holds is \
                refused with window_claimed."
                .into(),
            input_schema: json!({"type":"object","required":["pid","window_id","holder"],"properties":{
                "pid":{"type":"integer","description":"Process that owns the window."},
                "window_id":{"type":"integer","description":"The window to hold."},
                "holder":{"type":"string","maxLength":HOLDER_MAX_CHARS,
                    "description":"Name of the agent that holds the window, shown to other agents."}
            },"additionalProperties":false}),
            read_only: false,
            destructive: false,
            idempotent: true,
            open_world: false,
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let (Some(pid), Some(window_id)) = (
            args.get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 0),
            args.get("window_id")
                .and_then(Value::as_u64)
                .filter(|id| *id > 0),
        ) else {
            return ToolResult::error("claim_window needs a positive pid and window_id.");
        };
        let Some(holder) = holder_name(args.get("holder").and_then(Value::as_str)) else {
            return ToolResult::error("claim_window needs a holder name.");
        };
        let owner_pid =
            tokio::task::spawn_blocking(move || crate::win32::window_owner_pid(window_id))
                .await
                .ok()
                .flatten();
        if let Err(result) = super::impl_::exact_window_ownership_result(pid, window_id, owner_pid)
        {
            return result;
        }
        let claimed = {
            let holder = holder.clone();
            tokio::task::spawn_blocking(move || window_claims::claim(window_id, pid, &holder)).await
        };
        match claimed {
            Ok(Ok(())) => ToolResult::text(format!("Holding window {window_id} as {holder}."))
                .with_structured(json!({
                    "status": "claimed",
                    "pid": pid,
                    "window_id": window_id,
                    "holder": holder,
                })),
            Ok(Err(other)) => claimed_refusal(window_id, &other),
            Err(error) => ToolResult::error(format!("claim_window: blocking task failed: {error}")),
        }
    }
}

// ── release_window ────────────────────────────────────────────────────────────

pub struct ReleaseWindowTool;
static RELEASE_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for ReleaseWindowTool {
    fn def(&self) -> &ToolDef {
        RELEASE_DEF.get_or_init(|| ToolDef {
            name: "release_window".into(),
            description: "Release a window this driver process holds with claim_window, so other \
                agents can act on it again. status is not_held when this process did not hold it."
                .into(),
            input_schema: json!({"type":"object","required":["window_id"],"properties":{
                "window_id":{"type":"integer","description":"The window to release."}
            },"additionalProperties":false}),
            read_only: false,
            destructive: false,
            idempotent: true,
            open_world: false,
        })
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let Some(window_id) = args
            .get("window_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
        else {
            return ToolResult::error("release_window needs a positive window_id.");
        };
        let released = window_claims::release(window_id);
        ToolResult::text(if released {
            format!("Released window {window_id}.")
        } else {
            format!("This process did not hold window {window_id}.")
        })
        .with_structured(json!({
            "status": if released { "released" } else { "not_held" },
            "window_id": window_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_names_are_cleaned_and_bounded() {
        assert_eq!(
            holder_name(Some("  Pi agent\u{7}  ")).as_deref(),
            Some("Pi agent")
        );
        assert_eq!(holder_name(Some(" \n ")), None);
        assert_eq!(holder_name(None), None);
        assert_eq!(
            holder_name(Some(&"x".repeat(200))).map(|name| name.chars().count()),
            Some(HOLDER_MAX_CHARS)
        );
    }

    #[test]
    fn calls_name_their_window_by_window_id_or_target() {
        assert_eq!(named_window(&json!({"pid": 1, "window_id": 77})), Some(77));
        assert_eq!(
            named_window(&json!({"target": {"kind": "window", "pid": 1, "window_id": 78}})),
            Some(78)
        );
        assert_eq!(named_window(&json!({"element_token": "t"})), None);
        assert_eq!(named_window(&json!({"window_id": 0})), None);
    }

    #[test]
    fn a_claimed_window_refusal_names_the_holder() {
        let result = claimed_refusal(77, "Pi agent 4f2a");
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], "window_claimed");
        assert_eq!(structured["status"], "refused");
        assert_eq!(structured["effect"], "refused");
        assert_eq!(structured["holder"], "Pi agent 4f2a");
        assert_eq!(structured["refusal"]["code"], "window_claimed");
    }
}
