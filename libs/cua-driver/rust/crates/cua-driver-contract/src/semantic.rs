//! Explicit accessibility-only writes. Completion is not application-effect evidence.
use crate::{Platform, SchemaMode, ToolAnnotations, ToolContract, ToolInput, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An exact semantic intent, not an action ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticOperation {
    Invoke,
    Select,
    SetValue,
}

/// Snapshot-bound Windows UIA operation. Physical and foreground delivery are not accepted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticActionInput {
    pub pid: u32,
    pub window_id: u64,
    pub element_token: String,
    pub operation: SemanticOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Repeat a public session label for multi-call work; omit for the transport's implicit session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}
impl ToolInput for SemanticActionInput {
    const TOOL_NAME: &'static str = "semantic_action";
    fn validate(&self) -> Result<(), String> {
        if self.pid == 0
            || self.pid > i32::MAX as u32
            || self.window_id == 0
            || self.element_token.trim().is_empty()
        {
            return Err(
                "semantic_action requires an exact pid, window_id and current element_token".into(),
            );
        }
        if (self.operation == SemanticOperation::SetValue) != self.value.is_some() {
            return Err("value is required only for set_value".into());
        }
        Ok(())
    }
}

/// Actual native pattern that completed, not an inferred observed action label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticPattern {
    Invoke,
    SelectionItem,
    Value,
    RangeValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRoute {
    Accessibility,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEffect {
    Unverifiable,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDeliveryMode {
    Background,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticDelivery {
    pub mode: SemanticDeliveryMode,
}

/// Provider completion only. Clients must read back app state and measure focus independently.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticActionOutput {
    pub operation: SemanticOperation,
    pub pattern: SemanticPattern,
    pub route: SemanticRoute,
    pub effect: SemanticEffect,
    pub delivery: SemanticDelivery,
}
impl ToolOutput for SemanticActionOutput {
    fn validate(&self) -> Result<(), String> {
        let valid = matches!(
            (self.operation, self.pattern),
            (SemanticOperation::Invoke, SemanticPattern::Invoke)
                | (SemanticOperation::Select, SemanticPattern::SelectionItem)
                | (
                    SemanticOperation::SetValue,
                    SemanticPattern::Value | SemanticPattern::RangeValue
                )
        );
        if valid {
            Ok(())
        } else {
            Err("pattern must match the requested semantic operation".into())
        }
    }
}

pub(crate) fn contracts() -> Vec<ToolContract> {
    vec![ToolContract {
        name: SemanticActionInput::TOOL_NAME.into(),
        description: "Perform exactly invoke, select or set_value through an admitted Windows UIA pattern. Never use mouse/key events, activate the target or retry in foreground. Completion requires independent app readback; provider focus behavior is not guaranteed.".into(),
        platforms: vec![Platform::Windows], aliases: vec![],
        capabilities: vec!["accessibility.semantic_action".into(), "accessibility.element_tokens".into()],
        annotations: ToolAnnotations { read_only:false, destructive:true, idempotent:false, open_world:true },
        schema_mode: SchemaMode::CanonicalRuntime, cursor_semantics:None,
        input_schema: SemanticActionInput::input_schema(),
        success_output_schema: Some(SemanticActionOutput::output_schema()),
        error_output_schema: None,
        output_validator: crate::validate_typed_output::<SemanticActionOutput>,
    }]
}
