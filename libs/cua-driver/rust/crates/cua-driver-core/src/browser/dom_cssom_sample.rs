//! A fixed isolated-world CSSOM sample. It never supplies a native-pixel transform.

use serde_json::{json, Value};

/// Driver-owned world for readonly element geometry, without universal access.
pub(crate) const WORLD_NAME: &str = "cua-driver-dom-geometry";

/// Run with the attested unique execution context and one resolved node argument.
/// The caller must forbid side effects and revalidate the source around this read.
pub(crate) const READ_FUNCTION: &str = r#"function(node) {
    const connected = Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected').get.call(node);
    const same_document = Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument').get.call(node) === document;
    if (!connected || !same_document) return {connected, same_document};
    const rect = Element.prototype.getBoundingClientRect.call(node);
    const viewport = window.visualViewport;
    if (!viewport) throw new Error('visual viewport unavailable');
    return {
        connected,
        same_document,
        bounds: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
        scroll: {x: window.scrollX, y: window.scrollY},
        visual_viewport: {
            offset_x: viewport.offsetLeft, offset_y: viewport.offsetTop,
            page_x: viewport.pageLeft, page_y: viewport.pageTop,
            width: viewport.width, height: viewport.height, scale: viewport.scale
        }
    };
}"#;

/// Validate the full Runtime.callFunctionOn reply and retain only sampled fields.
pub(crate) fn parse_sample(reply: &Value) -> Result<Value, &'static str> {
    const UNAVAILABLE: &str = "dom_geometry_unavailable";
    if reply.get("exceptionDetails").is_some()
        || reply.get("wasThrown").and_then(Value::as_bool) == Some(true)
    {
        return Err(UNAVAILABLE);
    }
    let result = reply
        .get("result")
        .and_then(Value::as_object)
        .ok_or(UNAVAILABLE)?;
    if result
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("object"))
        || result.get("subtype").and_then(Value::as_str) == Some("error")
        || result.get("unserializableValue").is_some()
    {
        return Err(UNAVAILABLE);
    }
    let sample = result
        .get("value")
        .and_then(Value::as_object)
        .ok_or(UNAVAILABLE)?;
    let connected = sample
        .get("connected")
        .and_then(Value::as_bool)
        .ok_or(UNAVAILABLE)?;
    let same_document = sample
        .get("same_document")
        .and_then(Value::as_bool)
        .ok_or(UNAVAILABLE)?;
    if !connected || !same_document {
        return Err("dom_geometry_source_detached");
    }
    let bounds = project_numbers(sample.get("bounds"), &["x", "y", "width", "height"])?;
    let scroll = project_numbers(sample.get("scroll"), &["x", "y"])?;
    let viewport = project_numbers(
        sample.get("visual_viewport"),
        &[
            "offset_x", "offset_y", "page_x", "page_y", "width", "height", "scale",
        ],
    )?;
    // Derived extents stay inside the same bound as individual coordinates.
    let number = |record: &Value, key: &str| record[key].as_f64().expect("validated number");
    if number(&bounds, "width") < 0.0
        || number(&bounds, "height") < 0.0
        || (number(&bounds, "x") + number(&bounds, "width")).abs() > MAX_CSS_NUMBER
        || (number(&bounds, "y") + number(&bounds, "height")).abs() > MAX_CSS_NUMBER
        || ["width", "height", "scale"]
            .iter()
            .any(|key| number(&viewport, key) <= 0.0)
    {
        return Err(UNAVAILABLE);
    }
    Ok(json!({
        "connected":true,"same_document":true,
        "bounds":bounds,"scroll":scroll,"visual_viewport":viewport
    }))
}

const MAX_CSS_NUMBER: f64 = 1_000_000_000.0;

fn project_numbers(value: Option<&Value>, fields: &[&str]) -> Result<Value, &'static str> {
    let record = value
        .and_then(Value::as_object)
        .ok_or("dom_geometry_unavailable")?;
    let mut projected = serde_json::Map::new();
    for field in fields {
        let number = record
            .get(*field)
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite() && number.abs() <= MAX_CSS_NUMBER)
            .ok_or("dom_geometry_unavailable")?;
        projected.insert((*field).to_owned(), json!(number));
    }
    Ok(Value::Object(projected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Value {
        json!({"result":{"type":"object","value":{
            "connected":true,"same_document":true,
            "bounds":{"x":-3.25,"y":40.5,"width":80.25,"height":30.5},
            "scroll":{"x":-5.5,"y":321.25},
            "visual_viewport":{"offset_x":1.25,"offset_y":2.5,
                "page_x":-4.25,"page_y":323.75,"width":640.5,"height":480.25,"scale":1.25}
        }}})
    }

    #[test]
    fn css_geometry_keeps_fractional_css_units_without_a_dpr_conversion() {
        let input = reply();
        assert_eq!(parse_sample(&input), Ok(input["result"]["value"].clone()));
        let mut zero = reply();
        zero["result"]["value"]["bounds"]["width"] = json!(0);
        zero["result"]["value"]["bounds"]["height"] = json!(0);
        assert_eq!(parse_sample(&zero).unwrap()["bounds"]["width"], 0.0);
    }

    #[test]
    fn detached_or_foreign_document_samples_refuse_without_retaining_geometry() {
        for field in ["connected", "same_document"] {
            let mut input = reply();
            input["result"]["value"][field] = json!(false);
            assert_eq!(parse_sample(&input), Err("dom_geometry_source_detached"));
        }
        assert_eq!(
            parse_sample(&json!({"result":{"value":{
                "connected":false,"same_document":true
            }}})),
            Err("dom_geometry_source_detached")
        );
    }

    #[test]
    fn exceptions_and_malformed_replies_never_become_geometry() {
        let mut exception = reply();
        exception["exceptionDetails"] = json!({"text":"side effect checking unavailable"});
        let mut wrong_type = reply();
        wrong_type["result"]["type"] = json!("string");
        let mut error_object = reply();
        error_object["result"]["subtype"] = json!("error");
        let mut unserializable = reply();
        unserializable["result"]["unserializableValue"] = json!("NaN");
        let mut thrown = reply();
        thrown["wasThrown"] = json!(true);
        for input in [
            json!({}),
            json!({"result":{"value":null}}),
            exception,
            wrong_type,
            error_object,
            unserializable,
            thrown,
            json!({"result":{"value":{"connected":true,"same_document":"true"}}}),
        ] {
            assert_eq!(parse_sample(&input), Err("dom_geometry_unavailable"));
        }
        for field in [
            "connected",
            "same_document",
            "bounds",
            "scroll",
            "visual_viewport",
        ] {
            let mut input = reply();
            input["result"]["value"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert_eq!(
                parse_sample(&input),
                Err("dom_geometry_unavailable"),
                "{field}"
            );
        }
    }

    #[test]
    fn invalid_dimensions_and_unbounded_or_nonfinite_numbers_refuse() {
        for (group, field, invalid) in [
            ("bounds", "x", json!(1_000_000_001.0)),
            ("bounds", "y", json!(-1_000_000_001.0)),
            ("bounds", "width", json!(-0.1)),
            ("bounds", "height", json!(-0.1)),
            ("bounds", "x", json!("10")),
            ("bounds", "y", json!(f64::NAN)),
            ("bounds", "width", json!(f64::INFINITY)),
            ("scroll", "x", json!(1_000_000_001.0)),
            ("scroll", "y", json!(-1_000_000_001.0)),
            ("visual_viewport", "width", json!(0)),
            ("visual_viewport", "height", json!(-1)),
            ("visual_viewport", "scale", json!(0)),
            ("visual_viewport", "scale", json!(1_000_000_001.0)),
            ("visual_viewport", "page_y", json!(true)),
            ("visual_viewport", "offset_x", json!(f64::NEG_INFINITY)),
        ] {
            let mut input = reply();
            input["result"]["value"][group][field] = invalid;
            assert_eq!(
                parse_sample(&input),
                Err("dom_geometry_unavailable"),
                "{group}.{field}"
            );
        }
        let mut overflowing_extent = reply();
        overflowing_extent["result"]["value"]["bounds"]["x"] = json!(1_000_000_000.0);
        assert_eq!(
            parse_sample(&overflowing_extent),
            Err("dom_geometry_unavailable")
        );
    }

    #[test]
    fn missing_numeric_fields_are_unknown_not_zero() {
        for (group, fields) in [
            ("bounds", &["x", "y", "width", "height"][..]),
            ("scroll", &["x", "y"][..]),
            (
                "visual_viewport",
                &[
                    "offset_x", "offset_y", "page_x", "page_y", "width", "height", "scale",
                ][..],
            ),
        ] {
            for field in fields {
                let mut input = reply();
                input["result"]["value"][group]
                    .as_object_mut()
                    .unwrap()
                    .remove(*field);
                assert_eq!(
                    parse_sample(&input),
                    Err("dom_geometry_unavailable"),
                    "{group}.{field}"
                );
            }
        }
    }

    #[test]
    fn unknown_metadata_and_raw_protocol_identity_do_not_survive_projection() {
        let clean = reply();
        let mut input = clean.clone();
        input["remoteObjectId"] = json!("foreign-object");
        input["result"]["objectId"] = json!("foreign-object");
        let sample = &mut input["result"]["value"];
        sample["extra"] = json!("x".repeat(100_000));
        sample["bounds"]["native_pixels"] = json!([1, 2, 3, 4]);
        sample["scroll"]["objectId"] = json!("raw-id");
        sample["visual_viewport"]["devicePixelRatio"] = json!(1.5);
        assert_eq!(parse_sample(&input), Ok(clean["result"]["value"].clone()));
        assert!(
            serde_json::to_string(&parse_sample(&input).unwrap())
                .unwrap()
                .len()
                < 512
        );
    }
}
