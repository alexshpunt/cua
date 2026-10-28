//! Windows presentation metadata. These tools never capture pixels or dispatch input.
use crate::{Platform, SchemaMode, ToolAnnotations, ToolContract, ToolOutput};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_ACTIVITY_GENERATION: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum VisualPreset {
    Off,
    Subtle,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum VisualQuality {
    Performance,
    Balanced,
    High,
}

fn opacity_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"number","minimum":0,"maximum":1})
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AppliedVisualSettings {
    preset: VisualPreset,
    observations: bool,
    previews: bool,
    pointer: bool,
    scroll_drag: bool,
    keyboard: bool,
    #[schemars(schema_with = "opacity_schema")]
    opacity: f64,
    quality: VisualQuality,
    reduced_motion: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ConfiguredStatus {
    Configured,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetAgentVisualsOutput {
    status: ConfiguredStatus,
    settings: AppliedVisualSettings,
}

impl ToolOutput for SetAgentVisualsOutput {
    fn validate(&self) -> Result<(), String> {
        if !self.settings.opacity.is_finite() || !(0.0..=1.0).contains(&self.settings.opacity) {
            return Err("invalid_visual_opacity".into());
        }
        Ok(())
    }

    fn output_schema() -> Value {
        crate::outputs::output_schema_with_additional_properties::<Self>(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ActivityAdmissionStatus {
    Queued,
    Stale,
    Cleared,
    Disabled,
}

fn generation_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":"integer","minimum":1,"maximum":MAX_ACTIVITY_GENERATION})
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PresentAgentActivityOutput {
    status: ActivityAdmissionStatus,
    #[schemars(schema_with = "generation_schema")]
    generation: u64,
}

impl ToolOutput for PresentAgentActivityOutput {
    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_ACTIVITY_GENERATION).contains(&self.generation) {
            return Err("invalid_activity_generation".into());
        }
        Ok(())
    }

    fn output_schema() -> Value {
        crate::outputs::output_schema_with_additional_properties::<Self>(false)
    }
}

fn rect() -> Value {
    json!({"type":"object","required":["x","y","width","height"],"properties":{
    "x":{"type":"integer","minimum":-2000000,"maximum":2000000},"y":{"type":"integer","minimum":-2000000,"maximum":2000000},
    "width":{"type":"integer","minimum":1,"maximum":32768},"height":{"type":"integer","minimum":1,"maximum":32768}},"additionalProperties":false})
}
fn tool<O: ToolOutput>(
    name: &str,
    description: &str,
    capability: &str,
    schema: Value,
) -> ToolContract {
    ToolContract {
        name: name.into(),
        description: description.into(),
        platforms: vec![Platform::Windows],
        aliases: vec![],
        capabilities: vec![capability.into()],
        annotations: ToolAnnotations {
            read_only: false,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        schema_mode: SchemaMode::CanonicalRuntime,
        cursor_semantics: None,
        input_schema: schema,
        success_output_schema: Some(O::output_schema()),
        error_output_schema: None,
        output_validator: crate::validate_typed_output::<O>,
    }
}
pub fn contracts() -> Vec<ToolContract> {
    vec![
        tool::<SetAgentVisualsOutput>("set_agent_visuals","Configure the current session's human-only native activity presentation. Off clears pending visuals without changing capture or input.","agent_visuals.configure",json!({
            "type":"object","properties":{
                "session":{"type":"string"},"preset":{"type":"string","enum":["off","subtle","full"]},
                "observations":{"type":"boolean"},"previews":{"type":"boolean"},"pointer":{"type":"boolean"},"scroll_drag":{"type":"boolean"},"keyboard":{"type":"boolean"},
                "opacity":{"type":"number","minimum":0,"maximum":1},"quality":{"type":"string","enum":["performance","balanced","high"]},"reduced_motion":{"type":"boolean"}
            },"additionalProperties":false})),
        tool::<PresentAgentActivityOutput>("present_agent_activity","Enqueue validated human-only observation metadata and an optional operation-output image. Queued is not proof of display or action success. Clear invalidates older generations.","agent_visuals.present",json!({
            "type":"object","required":["generation","kind"],"properties":{
                "session":{"type":"string"},"generation":{"type":"integer","minimum":1,"maximum":9007199254740991u64},
                "kind":{"type":"string","enum":["overview","detail","inspect","verify","action","clear"]},
                "source":{"type":"object","required":["bounds"],"properties":{
                    "window_id":{"type":"integer","minimum":1,"maximum":9007199254740991u64},"pid":{"type":"integer","minimum":1},"identity":{"type":"string","maxLength":128},
                    "display":{"type":"string","minLength":1,"maxLength":256},"bounds":rect()},"additionalProperties":false},
                "region":rect(),"point":{"type":"object","required":["x","y"],"properties":{"x":{"type":"integer"},"y":{"type":"integer"}},"additionalProperties":false},
                "image":{"type":"object","required":["data","mime_type"],"properties":{"data":{"type":"string","minLength":1,"maxLength":12582912},"mime_type":{"type":"string","enum":["image/png","image/jpeg"]}},"additionalProperties":false},
                "action":{"type":"string","maxLength":64},"outcome":{"type":"string","enum":["delivered","refused","unknown","verified"]}
            },"additionalProperties":false})),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_payload() -> Value {
        json!({"status":"configured","settings":{
            "preset":"full","observations":true,"previews":true,"pointer":true,
            "scroll_drag":true,"keyboard":true,"opacity":0.5,"quality":"balanced",
            "reduced_motion":false
        }})
    }

    #[test]
    fn activity_acknowledgements_validate_admission_states_and_generation_bounds() {
        for status in ["queued", "stale", "cleared", "disabled"] {
            for generation in [1, 9_007_199_254_740_991u64] {
                let payload = json!({"status":status,"generation":generation});
                assert_eq!(
                    crate::validate_success_output("present_agent_activity", payload.clone()),
                    Ok(true)
                );
                let typed: PresentAgentActivityOutput =
                    serde_json::from_value(payload.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(), payload);
            }
        }
        for payload in [
            json!({"status":"delivered","generation":1}),
            json!({"status":"queued","generation":0}),
            json!({"status":"queued","generation":9_007_199_254_740_992u64}),
            json!({"status":"queued","generation":-1}),
            json!({"status":"queued","generation":1.5}),
            json!({"status":"queued","generation":"1"}),
            json!({"status":"queued"}),
            json!({"status":"queued","generation":1,"delivered":true}),
        ] {
            assert!(
                crate::validate_success_output("present_agent_activity", payload.clone()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn applied_visual_settings_require_the_complete_validated_wire_shape() {
        for preset in ["off", "subtle", "full"] {
            for quality in ["performance", "balanced", "high"] {
                for opacity in [0.0, 0.5, 1.0] {
                    let mut payload = configured_payload();
                    payload["settings"]["preset"] = json!(preset);
                    payload["settings"]["quality"] = json!(quality);
                    payload["settings"]["opacity"] = json!(opacity);
                    assert_eq!(
                        crate::validate_success_output("set_agent_visuals", payload.clone()),
                        Ok(true)
                    );
                    let typed: SetAgentVisualsOutput =
                        serde_json::from_value(payload.clone()).unwrap();
                    assert_eq!(serde_json::to_value(typed).unwrap(), payload);
                }
            }
        }
        for (field, value) in [
            ("preset", json!("unknown")),
            ("quality", json!("unknown")),
            ("opacity", json!(-0.01)),
            ("opacity", json!(1.01)),
            ("opacity", json!(1.000_000_01)),
            ("opacity", json!(-1e-50)),
            ("opacity", Value::Null),
            ("keyboard", json!("true")),
            ("unexpected", json!(true)),
        ] {
            let mut payload = configured_payload();
            payload["settings"][field] = value;
            assert!(
                crate::validate_success_output("set_agent_visuals", payload.clone()).is_err(),
                "{payload}"
            );
        }
        let mut missing = configured_payload();
        missing["settings"]
            .as_object_mut()
            .unwrap()
            .remove("reduced_motion");
        assert!(crate::validate_success_output("set_agent_visuals", missing).is_err());
        let mut wrong_status = configured_payload();
        wrong_status["status"] = json!("queued");
        assert!(crate::validate_success_output("set_agent_visuals", wrong_status).is_err());
    }

    #[test]
    fn advertised_presentation_schemas_keep_strict_typed_success_and_refusal_arms() {
        for name in ["set_agent_visuals", "present_agent_activity"] {
            let success = crate::tool_success_output_schema(name).unwrap();
            let advertised = crate::advertised_tool_output_schema(name).unwrap();
            assert_eq!(success["additionalProperties"], false);
            assert_eq!(advertised["anyOf"][0], success);
            assert_eq!(advertised["anyOf"][1], crate::refusal_envelope_schema());
        }
        let present = crate::tool_success_output_schema("present_agent_activity").unwrap();
        assert_eq!(present["properties"]["generation"]["minimum"], 1);
        assert_eq!(
            present["properties"]["generation"]["maximum"],
            9_007_199_254_740_991u64
        );
    }
    #[test]
    fn native_presentation_has_no_input_or_capture_capability() {
        for contract in contracts() {
            assert_eq!(contract.platforms, vec![Platform::Windows]);
            assert!(contract
                .capabilities
                .iter()
                .all(|c| c.starts_with("agent_visuals.")));
            assert!(!contract.annotations.destructive);
            assert!(contract.input_schema["properties"]["text"].is_null());
        }
    }
}
