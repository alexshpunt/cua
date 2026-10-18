//! Versioned, bounded locator-action receipts. Acknowledgements are not application success.
use crate::{ActionDeliveryMode, ActionEffect, ActionResult, ActionRoute, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIMIT: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Wire identity of the prefix-preserving DOM receipt.
pub enum DomActionFormat {
    DomActionV1,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Whether the programmatic plan finished dispatch or stopped.
pub enum DomActionStatus {
    Dispatched,
    Refused,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Caller-selected intent; unsupported trusted input never downgrades.
pub enum DomActionInputRoute {
    DomEvent,
    Trusted,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Native-pixel mapping stays unavailable for DOM actions.
pub enum DomActionMapping {
    Unavailable,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Acknowledgements never verify an application-level effect.
pub enum DomActionEffects {
    Unverified,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Execution bookkeeping, not a claim about page state.
pub enum DomActionProgressStatus {
    Pending,
    Dispatched,
    Completed,
    Refused,
    Failed,
    Cancelled,
    Timeout,
    Unknown,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Whether only the request-owned cleanup could be confirmed.
pub enum DomActionCleanup {
    NotStarted,
    Confirmed,
    Unconfirmed,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Required limitations retained with every programmatic DOM receipt.
pub enum DomActionLimitation {
    ProgrammaticDomSemanticsOnly,
    NoCssHoverProof,
    NoTrustedKeyDefaults,
    NoApplicationSuccessProof,
}

/// Text-free prefixes retained even when the final native step has no acknowledgement.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomActionProgress {
    pub status: DomActionProgressStatus,
    #[schemars(length(max = 96))]
    pub reason: Option<String>,
    #[schemars(length(max = 32))]
    pub attempted_steps: Vec<String>,
    #[schemars(length(max = 32))]
    pub acknowledged_steps: Vec<String>,
    #[schemars(length(max = 64))]
    pub in_flight: Option<String>,
    pub mutation_attempted: bool,
    pub effects: DomActionEffects,
    #[schemars(schema_with = "false_schema")]
    pub automatic_retry: bool,
    pub cleanup: DomActionCleanup,
}

impl DomActionProgress {
    /// Validate a standalone diagnostic prefix; this never proves native or application effects.
    pub fn validate_prefix(&self) -> Result<(), String> {
        bounded_encoding(self)?;
        let acknowledged = self.acknowledged_steps.len();
        let attempted = self.attempted_steps.len();
        if self.automatic_retry
            || attempted > 32
            || acknowledged > attempted
            || attempted - acknowledged > 1
            || self.mutation_attempted != (attempted > 0)
            || self.attempted_steps.iter().any(|step| !step_name(step))
            || self.acknowledged_steps != self.attempted_steps[..acknowledged]
            || self.in_flight.as_deref()
                != self.attempted_steps.get(acknowledged).map(String::as_str)
            || self
                .reason
                .as_deref()
                .is_some_and(|reason| !identifier(reason, 96))
            || self.status == DomActionProgressStatus::Pending
            || (self.status == DomActionProgressStatus::Unknown && self.in_flight.is_none())
            || (attempted > 0 && self.cleanup == DomActionCleanup::NotStarted)
        {
            return Err("invalid_dom_action_result".to_owned());
        }
        Ok(())
    }
    /// Decode only bounded, closed prefix data for an explicitly unverified error diagnostic.
    pub fn from_value(value: Value) -> Result<Self, String> {
        bounded_encoding(&value)?;
        let prefix: Self =
            serde_json::from_value(value).map_err(|_| "invalid_dom_action_result".to_owned())?;
        prefix.validate_prefix()?;
        Ok(prefix)
    }
}

/// Closed native observations. No text values, locator data, native IDs or geometry survive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomActionObserved {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub activation_invoked: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub focused: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub scroll_requested: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub already_matched: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub checked_matches: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub value_matches: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub text_applied: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub selected_matches: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_bool"
    )]
    #[schemars(schema_with = "boolean_schema")]
    pub event_cancelled: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// One acknowledged step's bounded boolean sample, not a live state object.
pub struct DomActionObservation {
    #[schemars(length(min = 1, max = 64))]
    pub step: String,
    pub observed: DomActionObserved,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Bounded refusal metadata without protocol or target identity.
pub struct DomActionRefusal {
    #[schemars(length(min = 1, max = 96))]
    pub code: String,
    #[schemars(length(max = 1024))]
    pub message: String,
}

/// Extended action output; its closed legacy projection never replaces the prefix receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomActionResult {
    pub format: DomActionFormat,
    pub status: DomActionStatus,
    #[schemars(schema_with = "nullable_input_route_schema")]
    pub input_route: Option<DomActionInputRoute>,
    #[schemars(schema_with = "false_schema")]
    pub trusted: bool,
    #[schemars(schema_with = "false_schema")]
    pub atomic: bool,
    pub native_pixel_mapping: DomActionMapping,
    pub action_result: ActionResult,
    pub progress: DomActionProgress,
    #[schemars(length(max = 32))]
    pub observed: Vec<DomActionObservation>,
    pub refusal: Option<DomActionRefusal>,
    #[schemars(length(min = 4, max = 4))]
    pub limitations: Vec<DomActionLimitation>,
}

impl DomActionResult {
    /// Advertise the extended receipt separately from pre-dispatch registry refusals.
    /// A payload carrying receipt fields cannot hide in the generic refusal arm.
    pub fn advertised_output_schema() -> Value {
        let mut refusal = crate::refusal_envelope_schema();
        refusal["not"] = serde_json::json!({"anyOf":[
            {"required":["format"]},{"required":["action_result"]},{"required":["observed"]},
            {"required":["input_route"]},{"required":["trusted"]},{"required":["atomic"]},
            {"required":["native_pixel_mapping"]},{"required":["limitations"]}
        ]});
        refusal["allOf"] = serde_json::json!([{"if":{"required":["progress"]},
            "then":{"required":["code","execution_state"],"properties":{"execution_state":{"const":"unknown"}}}}]);
        serde_json::json!({"type":"object","anyOf":[Self::output_schema(),refusal]})
    }
    /// Decode only a bounded versioned receipt, then validate its cross-field invariants.
    pub fn from_value(value: Value) -> Result<Self, String> {
        bounded_encoding(&value)?;
        let result: Self =
            serde_json::from_value(value).map_err(|_| "invalid_dom_action_result".to_owned())?;
        result.validate()?;
        Ok(result)
    }
}
impl ToolOutput for DomActionResult {
    fn validate(&self) -> Result<(), String> {
        bounded_encoding(self)?;
        let invalid = || "invalid_dom_action_result".to_owned();
        let p = &self.progress;
        let acknowledged = p.acknowledged_steps.len();
        let attempted = p.attempted_steps.len();
        let dispatched = self.status == DomActionStatus::Dispatched;
        p.validate_prefix()?;
        if self.trusted
            || self.atomic
            || (attempted > 0 && self.input_route != Some(DomActionInputRoute::DomEvent))
            || self.observed.len() > acknowledged
            || self
                .observed
                .iter()
                .enumerate()
                .any(|(index, sample)| sample.step != p.acknowledged_steps[index])
            || self.limitations
                != [
                    DomActionLimitation::ProgrammaticDomSemanticsOnly,
                    DomActionLimitation::NoCssHoverProof,
                    DomActionLimitation::NoTrustedKeyDefaults,
                    DomActionLimitation::NoApplicationSuccessProof,
                ]
        {
            return Err(invalid());
        }
        if dispatched {
            if attempted == 0
                || p.in_flight.is_some()
                || p.reason.is_some()
                || self.refusal.is_some()
                || !matches!(
                    p.status,
                    DomActionProgressStatus::Dispatched | DomActionProgressStatus::Completed
                )
                || p.cleanup != DomActionCleanup::Confirmed
                || self.observed.len() != acknowledged
            {
                return Err(invalid());
            }
        } else {
            let refusal = self.refusal.as_ref().ok_or_else(invalid)?;
            if matches!(
                p.status,
                DomActionProgressStatus::Dispatched | DomActionProgressStatus::Completed
            ) || p.reason.is_none()
                || !identifier(&refusal.code, 96)
                || refusal.message.len() > 1024
            {
                return Err(invalid());
            }
        }
        self.action_result
            .validate_invariants()
            .map_err(|_| invalid())?;
        let action = &self.action_result;
        if action.route != ActionRoute::Dom
            || action.summary.is_some()
            || action.evidence.is_some()
            || action.escalation.is_some()
        {
            return Err(invalid());
        }
        if attempted > 0 {
            let delivery = action.delivery.as_ref().ok_or_else(invalid)?;
            if action.effect != ActionEffect::Unverifiable
                || action.error.is_some()
                || delivery.mode != ActionDeliveryMode::Unknown
                || delivery.delivered_count.is_some()
            {
                return Err(invalid());
            }
        } else {
            let refusal = self.refusal.as_ref().ok_or_else(invalid)?;
            let error = action.error.as_ref().ok_or_else(invalid)?;
            if action.effect != ActionEffect::Refused
                || action.delivery.is_some()
                || error.code != refusal.code
                || error.hint.as_deref() != Some(refusal.message.as_str())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn output_schema() -> Value {
        crate::outputs::output_schema_with_additional_properties::<Self>(false)
    }
}

fn nullable_input_route_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let route = generator.subschema_for::<DomActionInputRoute>();
    schemars::json_schema!({"anyOf": [route, {"type":"null"}]})
}
fn present_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    bool::deserialize(deserializer).map(Some)
}
fn boolean_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"boolean"})
}
fn false_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"boolean","const":false})
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}
fn step_name(value: &str) -> bool {
    identifier(value, 64)
        && matches!(
            value,
            "click"
                | "dblclick"
                | "contextmenu"
                | "pointerover"
                | "mouseover"
                | "pointerenter"
                | "mouseenter"
                | "focus"
                | "scroll_into_view"
                | "set_checked"
                | "beforeinput"
                | "fill"
                | "insert_text"
                | "input"
                | "keydown"
                | "keyup"
                | "select_values"
                | "change"
        )
}
fn bounded_encoding(value: &impl Serialize) -> Result<(), String> {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::other(
                    "DOM action receipt exceeds its bound",
                ));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(LIMIT), value).map_err(|_| "invalid_dom_action_result".to_owned())
}
