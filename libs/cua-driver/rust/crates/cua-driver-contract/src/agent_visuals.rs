//! Windows presentation metadata. These tools never capture pixels or dispatch input.
use crate::{Platform, SchemaMode, ToolAnnotations, ToolContract};
use serde_json::{json, Value};

fn rect() -> Value {
    json!({"type":"object","required":["x","y","width","height"],"properties":{
    "x":{"type":"integer","minimum":-2000000,"maximum":2000000},"y":{"type":"integer","minimum":-2000000,"maximum":2000000},
    "width":{"type":"integer","minimum":1,"maximum":32768},"height":{"type":"integer","minimum":1,"maximum":32768}},"additionalProperties":false})
}
fn tool(name: &str, description: &str, capability: &str, schema: Value) -> ToolContract {
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
        success_output_schema: None,
        error_output_schema: None,
        output_validator: crate::default_output_validator(),
    }
}
pub fn contracts() -> Vec<ToolContract> {
    vec![
        tool("set_agent_visuals","Configure the current session's human-only native activity presentation. Off clears pending visuals without changing capture or input.","agent_visuals.configure",json!({
            "type":"object","properties":{
                "session":{"type":"string"},"preset":{"type":"string","enum":["off","subtle","full"]},
                "observations":{"type":"boolean"},"previews":{"type":"boolean"},"pointer":{"type":"boolean"},"scroll_drag":{"type":"boolean"},"keyboard":{"type":"boolean"},
                "opacity":{"type":"number","minimum":0,"maximum":1},"quality":{"type":"string","enum":["performance","balanced","high"]},"reduced_motion":{"type":"boolean"}
            },"additionalProperties":false})),
        tool("present_agent_activity","Enqueue validated human-only observation metadata and an optional operation-output image. Queued is not proof of display or action success. Clear invalidates older generations.","agent_visuals.present",json!({
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
