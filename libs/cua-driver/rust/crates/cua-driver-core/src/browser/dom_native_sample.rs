//! One attested CSSOM sample for a separate, opt-in native geometry candidate.
use super::{dom_cssom_sample, dom_geometry::CssViewport, types::Rect};
use serde_json::{json, Value};

pub(crate) const WORLD_NAME: &str = "cua-driver-dom-native-geometry";

/// Driver-owned code only. Resolve the node in this exact isolated context,
/// forbid side effects, and independently bracket this read with native/source checks.
pub(crate) const READ_FUNCTION: &str = r#"function(node) {
    const connected = Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected').get.call(node);
    const same_document = Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument').get.call(node) === document;
    if (!connected || !same_document) return {connected, same_document};
    const rect = Element.prototype.getBoundingClientRect.call(node);
    const viewport = window.visualViewport;
    if (!viewport) throw new Error('visual viewport unavailable');
    return {
        connected, same_document,
        bounds: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
        scroll: {x: window.scrollX, y: window.scrollY},
        layout_viewport: {width: window.innerWidth, height: window.innerHeight, dpr: window.devicePixelRatio},
        visual_viewport: {
            offset_x: viewport.offsetLeft, offset_y: viewport.offsetTop,
            page_x: viewport.pageLeft, page_y: viewport.pageTop,
            width: viewport.width, height: viewport.height, scale: viewport.scale
        }
    };
}"#;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeSample(pub Value);
impl NativeSample {
    pub(crate) fn bounds(&self) -> Rect {
        Rect {
            x: self.0["bounds"]["x"].as_f64().unwrap(),
            y: self.0["bounds"]["y"].as_f64().unwrap(),
            width: self.0["bounds"]["width"].as_f64().unwrap(),
            height: self.0["bounds"]["height"].as_f64().unwrap(),
        }
    }
    pub(crate) fn viewport(&self) -> CssViewport {
        CssViewport {
            width: self.0["layout_viewport"]["width"].as_f64().unwrap(),
            height: self.0["layout_viewport"]["height"].as_f64().unwrap(),
            dpr: self.0["layout_viewport"]["dpr"].as_f64().unwrap(),
            visual_scale: self.0["visual_viewport"]["scale"].as_f64().unwrap(),
            offset_x: self.0["visual_viewport"]["offset_x"].as_f64().unwrap(),
            offset_y: self.0["visual_viewport"]["offset_y"].as_f64().unwrap(),
        }
    }
}

pub(crate) fn parse_sample(reply: &Value) -> Result<NativeSample, &'static str> {
    let mut value = dom_cssom_sample::parse_sample(reply)?;
    let input = &reply["result"]["value"]["layout_viewport"];
    let number = |key: &str| {
        input[key]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0 && *n <= 1_000_000_000.0)
            .ok_or("dom_native_geometry_unavailable")
    };
    let width = number("width")?;
    let height = number("height")?;
    let dpr = number("dpr")?;
    if width.fract() != 0.0 || height.fract() != 0.0 || !(0.1..=16.0).contains(&dpr) {
        return Err("dom_native_geometry_unavailable");
    }
    if value["visual_viewport"]["scale"].as_f64() != Some(1.0)
        || value["visual_viewport"]["offset_x"].as_f64() != Some(0.0)
        || value["visual_viewport"]["offset_y"].as_f64() != Some(0.0)
        || value["visual_viewport"]["page_x"].as_f64() != value["scroll"]["x"].as_f64()
        || value["visual_viewport"]["page_y"].as_f64() != value["scroll"]["y"].as_f64()
    {
        return Err("dom_visual_viewport_unqualified");
    }
    value["layout_viewport"] = json!({"width":width,"height":height,"dpr":dpr});
    Ok(NativeSample(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply() -> Value {
        json!({"result":{"type":"object","value":{
            "connected":true,"same_document":true,
            "bounds":{"x":10.5,"y":20.25,"width":12.0,"height":10.0},
            "scroll":{"x":0.0,"y":300.5},
            "layout_viewport":{"width":886,"height":546,"dpr":1.5},
            "visual_viewport":{"offset_x":0.0,"offset_y":0.0,"page_x":0.0,"page_y":300.5,
                "width":886.0,"height":546.0,"scale":1.0}
        }}})
    }
    #[test]
    fn controlled_scroll_keeps_layout_relative_border_box() {
        let sample = parse_sample(&reply()).unwrap();
        assert_eq!(sample.bounds().y, 20.25);
        assert_eq!(sample.0["scroll"]["y"], 300.5);
        assert_eq!(sample.viewport().dpr, 1.5);
    }
    #[test]
    fn zoom_offsets_and_inconsistent_scroll_refuse() {
        for (key, value) in [
            ("scale", 1.25),
            ("offset_x", 0.1),
            ("offset_y", 1.0),
            ("page_y", 300.0),
        ] {
            let mut reply = reply();
            reply["result"]["value"]["visual_viewport"][key] = json!(value);
            assert_eq!(parse_sample(&reply), Err("dom_visual_viewport_unqualified"));
        }
    }
    #[test]
    fn missing_or_nonfinite_layout_evidence_never_derives_a_scale() {
        for (key, value) in [
            ("width", json!(885.5)),
            ("height", json!(0)),
            ("dpr", Value::Null),
            ("dpr", json!(f64::INFINITY)),
        ] {
            let mut reply = reply();
            reply["result"]["value"]["layout_viewport"][key] = value;
            assert_eq!(parse_sample(&reply), Err("dom_native_geometry_unavailable"));
        }
    }
}
