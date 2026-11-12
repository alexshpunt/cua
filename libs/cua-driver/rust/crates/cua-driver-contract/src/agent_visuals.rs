//! Windows presentation metadata. These tools never capture pixels or dispatch input.
use crate::{Platform, SchemaMode, ToolAnnotations, ToolContract, ToolOutput};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_ACTIVITY_GENERATION: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum VisualPreset {
    Off,
    Subtle,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum VisualQuality {
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

fn rect(description: &str) -> Value {
    json!({"type":"object","description":description,"required":["x","y","width","height"],"properties":{
    "x":{"type":"integer","minimum":-2000000,"maximum":2000000,"description":"Left edge in pixels."},
    "y":{"type":"integer","minimum":-2000000,"maximum":2000000,"description":"Top edge in pixels."},
    "width":{"type":"integer","minimum":1,"maximum":32768,"description":"Width in pixels."},
    "height":{"type":"integer","minimum":1,"maximum":32768,"description":"Height in pixels."}},"additionalProperties":false})
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
                "session":{"type":"string","description":"Public session label whose presentation settings change. Omit it to use the authenticated transport's implicit lifecycle session."},
                "preset":{"type":"string","enum":["off","subtle","full"],"description":"Presentation level; the default is full. off disables native activity visuals and clears pending ones; subtle shows them without operation-output previews; full also shows previews."},
                "observations":{"type":"boolean","description":"Show observation activity (overview, detail, inspect and verify). Default true."},
                "previews":{"type":"boolean","description":"Show operation-output images on activity cards. Takes effect only with preset full. Default true."},
                "pointer":{"type":"boolean","description":"Show pointer actions such as clicks. Default true."},
                "scroll_drag":{"type":"boolean","description":"Show scroll and drag actions. Default true."},
                "keyboard":{"type":"boolean","description":"Show keyboard actions such as typing text and pressing keys. Default true."},
                "opacity":{"type":"number","minimum":0,"maximum":1,"description":"Presentation opacity from 0 to 1; 0 disables the visuals. Default 0.9."},
                "quality":{"type":"string","enum":["performance","balanced","high"],"description":"Frame-rate tradeoff: performance renders at about 20 frames per second, balanced at about 30 and high at about 60. Default balanced."},
                "reduced_motion":{"type":"boolean","description":"Use reduced motion for the presentation's animation. Default false."}
            },"additionalProperties":false})),
        tool::<PresentAgentActivityOutput>("present_agent_activity","Enqueue validated human-only observation metadata and an optional operation-output image. Queued is not proof of display or action success. Clear invalidates older generations.","agent_visuals.present",json!({
            "type":"object","required":["generation","kind"],"properties":{
                "session":{"type":"string","description":"Public session label that owns the presented activity. Omit it to use the authenticated transport's implicit lifecycle session."},
                "generation":{"type":"integer","minimum":1,"maximum":9007199254740991u64,"description":"Monotonically increasing publication number within the session. A generation not newer than the latest accepted one is reported as stale."},
                "kind":{"type":"string","enum":["overview","detail","inspect","verify","action","clear"],"description":"What the activity shows: overview, detail, inspect or verify for an observation, action for an operation that already ran, or clear to remove the visuals and invalidate older generations. clear takes no source, region, point or image."},
                "source":{"type":"object","description":"Where the activity happened: a window (window_id, pid and identity) or a display (display), plus its bounds. Required unless kind is clear. A window source and a display source cannot be mixed.","required":["bounds"],"properties":{
                    "window_id":{"type":"integer","minimum":1,"maximum":9007199254740991u64,"description":"Window id, as returned by the window tools. Set it with pid and identity for a window source."},
                    "pid":{"type":"integer","minimum":1,"description":"Process id that owns the window. Must match the pid inside identity."},
                    "identity":{"type":"string","maxLength":128,"description":"Window ownership token in the form PiComputerUse.<32 hex digits>:<pid>:<counter>."},
                    "display":{"type":"string","minLength":1,"maxLength":256,"description":"Display id for a display source, such as \"primary\". Do not combine it with window fields."},
                    "bounds":rect("Pixel bounds of the source. The region and point must lie inside it.")},"additionalProperties":false},
                "region":rect("Area of interest inside the source bounds, in the same coordinates as the bounds."),
                "point":{"type":"object","description":"Point of interest inside the source bounds, in the same coordinates as the bounds.","required":["x","y"],"properties":{"x":{"type":"integer","description":"Horizontal position in pixels."},"y":{"type":"integer","description":"Vertical position in pixels."}},"additionalProperties":false},
                "image":{"type":"object","description":"Operation-output image already returned by an observation, shown as a preview. Nothing is captured. Not accepted for action activity or a refused or unknown outcome.","required":["data","mime_type"],"properties":{"data":{"type":"string","minLength":1,"maxLength":12582912,"description":"Base64-encoded image bytes."},"mime_type":{"type":"string","enum":["image/png","image/jpeg"],"description":"Image format of data."}},"additionalProperties":false},
                "action":{"type":"string","maxLength":64,"description":"Name of the operation that ran, in lowercase letters and underscores, such as click or type_text. It selects the card label and whether the pointer, keyboard or scroll_drag setting applies."},
                "typed_text":{"type":"string","minLength":1,"maxLength":200,"description":"Text an action typed, shown beside the action. Only for kind action; 1 to 200 characters without control characters."},
                "outcome":{"type":"string","enum":["delivered","refused","unknown","verified"],"description":"Result of the operation or observation: delivered, refused, unknown or verified."}
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
            // Typed text is display metadata of an action that already ran through its own tool.
            let typed = &contract.input_schema["properties"]["typed_text"];
            assert!(typed.is_null() || typed["maxLength"] == 200);
        }
    }
}
