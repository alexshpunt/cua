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
