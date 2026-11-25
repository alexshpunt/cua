// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Adapter-internal native visualization tools, separate from capture/input authority.
use super::*;

pub struct SetAgentVisualsTool;
pub struct PresentAgentActivityTool;
static SETTINGS_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
static PRESENT_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
fn canonical_cursor_def(name: &str) -> ToolDef {
    let contract = cua_driver_contract::tool_contract(name)
        .unwrap_or_else(|| panic!("missing canonical contract for {name}"));
    ToolDef::from_contract(&contract)
}
fn failure(code: &'static str) -> ToolResult {
    ToolResult::error("Native presentation is unavailable for this request.")
        .with_structured(json!({"status":"unavailable","code":code}))
}
#[async_trait]
impl Tool for SetAgentVisualsTool {
    fn def(&self) -> &ToolDef {
        SETTINGS_DEF.get_or_init(|| canonical_cursor_def("set_agent_visuals"))
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let key = resolve_cursor_key(&args);
        match crate::activity::configure(&key, args) {
            Ok(settings) => ToolResult::text("Native activity settings applied.")
                .with_structured(json!({"status":"configured","settings":settings})),
            Err(code) => failure(code),
        }
    }
}
#[async_trait]
impl Tool for PresentAgentActivityTool {
    fn def(&self) -> &ToolDef {
        PRESENT_DEF.get_or_init(|| canonical_cursor_def("present_agent_activity"))
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let key = resolve_cursor_key(&args);
        let event: cursor_overlay::activity::ActivityEvent = match serde_json::from_value(args) {
            Ok(event) => event,
            Err(_) => return failure("invalid_activity"),
        };
        let generation = event.generation;
        match crate::activity::present(&key, event) {
            Ok(status) => ToolResult::text("Native activity request processed.")
                .with_structured(json!({"status":status,"generation":generation})),
            Err(code) => failure(code),
        }
    }
}

pub struct DesktopActivityTool;
static DESKTOP_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

#[async_trait]
impl Tool for DesktopActivityTool {
    fn def(&self) -> &ToolDef {
        DESKTOP_DEF.get_or_init(|| canonical_cursor_def(cua_driver_core::desktop_activity::TOOL))
    }

    async fn invoke(&self, mut args: Value) -> ToolResult {
        let Some(owner) = args["_transport_session_id"]
            .as_str()
            .or_else(|| args["_session_id"].as_str())
            .map(str::to_owned)
        else {
            return failure("desktop_activity_unavailable");
        };
        if let Some(fields) = args.as_object_mut() {
            fields.retain(|key, _| !key.starts_with('_'));
        }
        let Ok(update) = serde_json::from_value(args) else {
            return failure("invalid_desktop_activity");
        };
        match crate::desktop_activity::apply(&owner, update) {
            Ok(changed) => {
                let status = if changed { "accepted" } else { "ignored" };
                ToolResult::text("Desktop activity metadata processed.")
                    .with_structured(json!({"status":status}))
            }
            Err(code) => failure(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn invalid_preview_refuses_without_reflecting_or_decoding_image_bytes() {
        let result=PresentAgentActivityTool.invoke(json!({"session":"activity-invalid","generation":1,"kind":"overview","image":{"data":"PRIVATE_SENTINEL","mime_type":3}})).await;
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "invalid_activity"
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("PRIVATE_SENTINEL"));
    }
}
