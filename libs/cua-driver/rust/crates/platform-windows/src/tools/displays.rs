//! Windows-only public monitor reads; no input capture publication or desktop-scale mutation.

use async_trait::async_trait;
use cua_driver_contract::{GetDisplayStateInput, ListDisplaysInput};
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::parse_typed_input,
};
use serde_json::{json, Value};

/// Native connected-monitor discovery without capture or input.
pub struct ListDisplaysTool;
/// Exact monitor capture without input-authorizing capture publication.
pub struct GetDisplayStateTool;
static LIST_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();
static STATE_DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

fn failure(error: anyhow::Error) -> ToolResult {
    let code = error
        .downcast_ref::<crate::display::DisplayReadError>()
        .map(|e| e.code())
        .unwrap_or("display_read_failed");
    ToolResult::error(format!("Windows display read failed: {error}"))
        .with_structured(json!({"status":"refused", "code":code, "message":error.to_string()}))
}

#[async_trait]
impl Tool for ListDisplaysTool {
    fn def(&self) -> &ToolDef {
        LIST_DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("list_displays").expect("display contract"),
            )
        })
    }
    async fn protected_resource_scope(
        &self,
        adapter: &str,
        _: &Value,
    ) -> Result<Option<Value>, String> {
        if adapter != "private_observation" {
            return Ok(None);
        }
        #[cfg(target_os = "windows")]
        {
            let current = tokio::task::spawn_blocking(crate::display::enumerate)
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            Ok(Some(
                json!({"kind":"windows_display_inventory", "topology_id":current.id}),
            ))
        }
        #[cfg(not(target_os = "windows"))]
        Err("Selected displays are supported only on Windows".into())
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        if let Err(error) = parse_typed_input::<ListDisplaysInput>("list_displays", args) {
            return error;
        }
        #[cfg(target_os = "windows")]
        {
            match tokio::task::spawn_blocking(crate::display::enumerate).await {
                Ok(Ok(current)) => ToolResult::text(format!("{} connected Windows displays", current.displays.len()))
                    .with_structured(json!({"platform":"windows", "topology_id":current.id, "displays":current.displays})),
                Ok(Err(error)) => failure(error),
                Err(error) => failure(error.into()),
            }
        }
        #[cfg(not(target_os = "windows"))]
        failure(anyhow::anyhow!(
            "Selected displays are supported only on Windows"
        ))
    }
}

#[async_trait]
impl Tool for GetDisplayStateTool {
    fn def(&self) -> &ToolDef {
        STATE_DEF.get_or_init(|| {
            ToolDef::from_contract(
                &cua_driver_contract::tool_contract("get_display_state").expect("display contract"),
            )
        })
    }

    async fn protected_resource_scope(
        &self,
        adapter: &str,
        args: &Value,
    ) -> Result<Option<Value>, String> {
        if adapter != "private_observation" {
            return Ok(None);
        }
        let input: GetDisplayStateInput = parse_typed_input("get_display_state", args.clone())
            .map_err(|_| "Invalid selected-display request".to_owned())?;
        #[cfg(target_os = "windows")]
        {
            let current = tokio::task::spawn_blocking(crate::display::enumerate)
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let selected = current
                .select(&input.display_id, &input.topology_id)
                .map_err(|e| e.to_string())?;
            Ok(Some(
                json!({"kind":"windows_display", "display_id":selected.display_id, "topology_id":current.id, "monitor_handle":selected.monitor_handle}),
            ))
        }
        #[cfg(not(target_os = "windows"))]
        Err("Selected displays are supported only on Windows".into())
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let input: GetDisplayStateInput = match parse_typed_input("get_display_state", args) {
            Ok(input) => input,
            Err(error) => return error,
        };
        #[cfg(target_os = "windows")]
        {
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<ToolResult> {
                use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
                use cua_driver_core::protocol::Content;
                let (display, png, overlay) = crate::display::capture(&input.display_id, &input.topology_id)?;
                let png = match input.max_image_dimension {
                    Some(cap) if cap > 0 => crate::capture::resize_png_if_needed(&png, cap)?,
                    _ => png,
                };
                let (width, height) = crate::capture::png_dimensions_pub(&png)?;
                let mut result = ToolResult::text(format!("Windows display {}: {width}x{height} local pixels, host origin {},{}; read-only", display.display_id, display.bounds.x, display.bounds.y))
                    .with_structured(json!({"platform":"windows", "topology_id":input.topology_id,
                        "screenshot_width":width, "screenshot_height":height,
                        "screenshot_original_width":display.bounds.width, "screenshot_original_height":display.bounds.height,
                        "screenshot_mime_type":"image/png", "display":display, "agent_overlay_capture":overlay}));
                result.content.insert(0, Content::image_png(BASE64.encode(png)));
                Ok(result)
            }).await;
            match result {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => failure(error),
                Err(error) => failure(error.into()),
            }
        }
        #[cfg(not(target_os = "windows"))]
        failure(anyhow::anyhow!(
            "Selected displays are supported only on Windows"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_display_tools_publish_live_schemas_without_startup_panic() {
        for tool in [
            &ListDisplaysTool as &dyn Tool,
            &GetDisplayStateTool as &dyn Tool,
        ] {
            let entry = tool.def().to_list_entry();
            let contract = cua_driver_contract::tool_contract(tool.def().name.as_str()).unwrap();
            assert_eq!(entry["inputSchema"], contract.input_schema);
            assert_eq!(entry["annotations"]["readOnlyHint"], true);
            assert!(entry["outputSchema"].is_object());
        }
    }
}
