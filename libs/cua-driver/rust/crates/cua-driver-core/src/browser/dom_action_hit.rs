//! Readonly document-CSS hit tests. No desktop/image coordinates or input authority.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HitProbe {
    Outside,
    Unknown,
    Point { x: i64, y: i64 },
}

/// Validate the point chosen by the fixed isolated-world sampler, not caller input.
pub(super) fn probe(reply: &Value) -> Result<HitProbe, &'static str> {
    let value = reply
        .pointer("/result/value/hit_test")
        .ok_or("dom_action_hit_unavailable")?;
    if value.is_null() {
        return Ok(HitProbe::Unknown);
    }
    if value == &Value::Bool(false) {
        return Ok(HitProbe::Outside);
    }
    let object = value.as_object().ok_or("dom_action_hit_unavailable")?;
    if object.len() != 6
        || !["x", "y", "scroll_x", "scroll_y", "width", "height"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return Err("dom_action_hit_unavailable");
    }
    let x = value["x"]
        .as_i64()
        .filter(|n| (-1_000_000_000..=1_000_000_000).contains(n))
        .ok_or("dom_action_hit_unavailable")?;
    let y = value["y"]
        .as_i64()
        .filter(|n| (-1_000_000_000..=1_000_000_000).contains(n))
        .ok_or("dom_action_hit_unavailable")?;
    for key in ["scroll_x", "scroll_y", "width", "height"] {
        let n = value[key]
            .as_f64()
            .filter(|n| n.is_finite() && n.abs() <= 1_000_000_000.0)
            .ok_or("dom_action_hit_unavailable")?;
        if matches!(key, "width" | "height") && n <= 0.0 {
            return Err("dom_action_hit_unavailable");
        }
    }
    let vx = x as f64 - value["scroll_x"].as_f64().unwrap();
    let vy = y as f64 - value["scroll_y"].as_f64().unwrap();
    let bounds = reply
        .pointer("/result/value/bounds")
        .and_then(Value::as_array)
        .filter(|r| r.len() == 4)
        .ok_or("dom_action_hit_unavailable")?;
    let b: Option<Vec<f64>> = bounds.iter().map(Value::as_f64).collect();
    let b = b
        .filter(|b| b.iter().all(|n| n.is_finite()))
        .ok_or("dom_action_hit_unavailable")?;
    if vx < 0.0
        || vy < 0.0
        || vx >= value["width"].as_f64().unwrap()
        || vy >= value["height"].as_f64().unwrap()
        || vx < b[0]
        || vy < b[1]
        || vx >= b[0] + b[2]
        || vy >= b[1] + b[3]
    {
        return Err("dom_action_hit_unavailable");
    }
    if x as f64 != (b[0] + b[2] / 2.0 + value["scroll_x"].as_f64().unwrap()).floor()
        || y as f64 != (b[1] + b[3] / 2.0 + value["scroll_y"].as_f64().unwrap()).floor()
    {
        return Err("dom_action_hit_unavailable");
    }
    Ok(HitProbe::Point { x, y })
}

/// Native hit identity must belong to this exact main document, never a child frame.
pub(super) fn hit_identity(reply: &Value, frame: &str) -> Result<i64, &'static str> {
    if reply.get("frameId").and_then(Value::as_str) != Some(frame) {
        return Err("dom_action_hit_frame_unavailable");
    }
    reply
        .get("backendNodeId")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .ok_or("dom_action_hit_unavailable")
}

/// Check ancestry only after resolving the native hit in the same attested world.
/// Closed/UA shadow ancestry, frames, detached nodes and exhausted paths stay unknown.
pub(super) const HIT_FUNCTION: &str = r#"function(node, hit) {
    const get = (prototype, name, receiver) => Object.getOwnPropertyDescriptor(prototype, name).get.call(receiver);
    if (!get(Node.prototype,'isConnected',node) || !get(Node.prototype,'isConnected',hit) ||
        get(Node.prototype,'ownerDocument',node) !== document || get(Node.prototype,'ownerDocument',hit) !== document)
        return {connected:false,same_document:false,relation:null};
    let current = hit, matched = false;
    for (let depth = 0; current && depth < 64; depth++) {
        if (current === document) return {connected:true,same_document:true,relation:matched};
        const type = get(Node.prototype,'nodeType',current);
        if (type === 1) {
            const tag = get(Element.prototype,'localName',current);
            if (['iframe','frame','object','embed'].includes(tag)) return {connected:true,same_document:true,relation:null};
            if (current === node) matched = true;
        } else if (type !== 11) return {connected:true,same_document:true,relation:null};
        const parent = get(Node.prototype,'parentNode',current);
        if (parent) current = parent;
        else if (current instanceof ShadowRoot && get(ShadowRoot.prototype,'mode',current) === 'open')
            current = get(ShadowRoot.prototype,'host',current);
        else return {connected:true,same_document:true,relation:null};
    }
    return {connected:true,same_document:true,relation:null};
}"#;

pub(super) fn relation(reply: &Value) -> Result<Option<bool>, &'static str> {
    if reply.pointer("/result/type").and_then(Value::as_str) != Some("object")
        || reply.pointer("/result/subtype").is_some()
    {
        return Err("dom_action_hit_unavailable");
    }
    let value = reply
        .pointer("/result/value")
        .and_then(Value::as_object)
        .ok_or("dom_action_hit_unavailable")?;
    if value.len() != 3
        || value.get("connected").and_then(Value::as_bool) != Some(true)
        || value.get("same_document").and_then(Value::as_bool) != Some(true)
    {
        return Err("dom_action_ref_stale");
    }
    match value.get("relation") {
        Some(Value::Null) => Ok(None),
        Some(Value::Bool(hit)) => Ok(Some(*hit)),
        _ => Err("dom_action_hit_unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn sample() -> Value {
        json!({"result":{"value":{"bounds":[10.5,20.25,80.0,30.0],"hit_test":{"x":50,"y":155,"scroll_x":0.0,"scroll_y":120.0,"width":800.0,"height":600.0}}}})
    }
    #[test]
    fn point_is_document_css_and_never_a_viewport_or_native_pixel_substitution() {
        assert_eq!(probe(&sample()), Ok(HitProbe::Point { x: 50, y: 155 }));
        let mut wrong = sample();
        wrong["result"]["value"]["hit_test"]["y"] = json!(35);
        assert_eq!(probe(&wrong), Err("dom_action_hit_unavailable"));
        let mut moved = sample();
        moved["result"]["value"]["hit_test"]["x"] = json!(51);
        assert_eq!(probe(&moved), Err("dom_action_hit_unavailable"));
        for bad in [
            json!(true),
            json!({}),
            json!({"x":50.1,"y":155,"scroll_x":0,"scroll_y":120,"width":800,"height":600}),
        ] {
            let mut value = sample();
            value["result"]["value"]["hit_test"] = bad;
            assert_eq!(probe(&value), Err("dom_action_hit_unavailable"));
        }
    }
    #[test]
    fn outside_unknown_wrong_frame_and_stale_hits_cannot_become_ready() {
        let mut value = sample();
        value["result"]["value"]["hit_test"] = Value::Null;
        assert_eq!(probe(&value), Ok(HitProbe::Unknown));
        value["result"]["value"]["hit_test"] = json!(false);
        assert_eq!(probe(&value), Ok(HitProbe::Outside));
        assert_eq!(
            hit_identity(&json!({"frameId":"child","backendNodeId":1}), "main"),
            Err("dom_action_hit_frame_unavailable")
        );
        assert_eq!(
            hit_identity(&json!({"frameId":"main","backendNodeId":0}), "main"),
            Err("dom_action_hit_unavailable")
        );
        assert_eq!(
            relation(
                &json!({"result":{"type":"object","value":{"connected":false,"same_document":true,"relation":true}}})
            ),
            Err("dom_action_ref_stale")
        );
    }
}
