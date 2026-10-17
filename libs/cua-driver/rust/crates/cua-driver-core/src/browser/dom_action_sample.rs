//! Fixed isolated-world actionability sampling. CSS boxes are never native-image geometry.

use super::dom_action_wait::{ActionState, ActionTarget};
use serde_json::Value;

/// Driver-owned world; the caller must attest its unique context and forbid universal access.
pub(crate) const WORLD_NAME: &str = "cua-driver-dom-actionability";

/// Run only with one resolved node in the attested isolated world, with side effects forbidden.
pub(crate) const READ_FUNCTION: &str = r#"function(node) {
    const get = (prototype, name, receiver) =>
        Object.getOwnPropertyDescriptor(prototype, name).get.call(receiver);
    const fact = read => { try { return read(); } catch (_) { return null; } };
    const connected = get(Node.prototype, 'isConnected', node);
    const same_document = get(Node.prototype, 'ownerDocument', node) === document;
    if (!connected || !same_document) return {connected, same_document};
    const rect = Element.prototype.getBoundingClientRect.call(node);
    const bounds = [rect.x, rect.y, rect.width, rect.height];
    const visible = fact(() => {
        const style = getComputedStyle(node);
        // A positive bounding rectangle cannot be the empty-client-rect fallback.
        // getClientRects is not side-effect-free in Blink's debugger bindings.
        return rect.width > 0 && rect.height > 0 && style.display !== 'none' &&
            style.visibility !== 'hidden' && style.visibility !== 'collapse';
    });
    const enabled = fact(() => {
        const roots = [document], pending = [document];
        let visited = 0;
        // Native getters bound the author/open-shadow walk before any selector query.
        while (pending.length) {
            if (++visited > 2000) return null;
            const current = pending.pop();
            const sibling = get(Node.prototype, 'nextSibling', current);
            const child = get(Node.prototype, 'firstChild', current);
            if (sibling) pending.push(sibling);
            if (child) pending.push(child);
            if (get(Node.prototype, 'nodeType', current) === 1) {
                const shadow = get(Element.prototype, 'shadowRoot', current);
                if (shadow) {
                    if (get(ShadowRoot.prototype, 'mode', shadow) !== 'open' || roots.length >= 128) return null;
                    roots.push(shadow); pending.push(shadow);
                }
            }
        }
        let disabled = false;
        for (const root of roots) {
            const query = root === document ? Document.prototype.querySelectorAll : DocumentFragment.prototype.querySelectorAll;
            const modal = query.call(root, ':modal');
            if (get(NodeList.prototype, 'length', modal) !== 0) return null;
            const rows = query.call(root, ':disabled');
            const count = get(NodeList.prototype, 'length', rows);
            if (!Number.isInteger(count) || count < 0 || count > 2000) return null;
            for (let i = 0; i < count; i++) if (NodeList.prototype.item.call(rows, i) === node) disabled = true;
        }
        if (disabled) return false;
        let ancestor = node;
        for (let depth = 0; ancestor && depth < 32; depth++) {
            let slot = null;
            if (get(Node.prototype, 'nodeType', ancestor) === 1) {
                if (Element.prototype.hasAttribute.call(ancestor, 'inert')) return false;
                const aria = Element.prototype.getAttribute.call(ancestor, 'aria-disabled');
                if (aria !== null && aria.trim().toLowerCase() === 'true') return false;
                // The named CSS getter is side-effect-free; CSS.supports and
                // getPropertyValue are not. Missing support remains unknown.
                const interactivity = getComputedStyle(ancestor).interactivity;
                if (interactivity === 'inert') return ancestor === node ? false : null;
                if (interactivity !== 'auto') return null;
                slot = get(Element.prototype, 'assignedSlot', ancestor);
            }
            if (ancestor === document) return true;
            const parent = get(Node.prototype, 'parentNode', ancestor);
            if (!parent && ancestor instanceof ShadowRoot) {
                if (get(ShadowRoot.prototype, 'mode', ancestor) !== 'open') return null;
                ancestor = get(ShadowRoot.prototype, 'host', ancestor);
            } else ancestor = slot || parent;
        }
        return null;
    });
    const editable = fact(() => {
        if (enabled === null) return null;
        if (!enabled) return false;
        if (node instanceof HTMLInputElement) {
            const type = get(HTMLInputElement.prototype, 'type', node);
            return ['text', 'search', 'tel', 'url', 'email', 'password', 'number'].includes(type) &&
                !get(HTMLInputElement.prototype, 'readOnly', node);
        }
        if (node instanceof HTMLTextAreaElement) return !get(HTMLTextAreaElement.prototype, 'readOnly', node);
        return node instanceof HTMLElement && get(HTMLElement.prototype, 'isContentEditable', node);
    });
    const hit_test = fact(() => {
        const viewport = window.visualViewport;
        if (!viewport || viewport.scale !== 1 || viewport.offsetLeft !== 0 || viewport.offsetTop !== 0) return null;
        if (rect.width <= 0 || rect.height <= 0) return false;
        const sx = window.scrollX, sy = window.scrollY;
        // DOM.getNodeForLocation consumes integral document-CSS coordinates.
        // Choose floor(center + scroll) once, then prove that point stays inside.
        const x = Math.floor(rect.x + rect.width / 2 + sx), y = Math.floor(rect.y + rect.height / 2 + sy);
        const vx = x - sx, vy = y - sy;
        if (!Number.isSafeInteger(x) || !Number.isSafeInteger(y) || Math.abs(x) > 1000000000 || Math.abs(y) > 1000000000) return null;
        if (vx < rect.x || vx >= rect.x + rect.width || vy < rect.y || vy >= rect.y + rect.height ||
            vx < 0 || vy < 0 || vx >= window.innerWidth || vy >= window.innerHeight) return false;
        return {x, y, scroll_x:sx, scroll_y:sy, width:window.innerWidth, height:window.innerHeight};
    });
    // Only CUA's separately source-qualified readonly native hit test can fill this fact.
    return {connected, same_document, bounds, visible, enabled, editable, receives_pointer:null, hit_test};
}"#;

const UNAVAILABLE: &str = "dom_action_state_unavailable";
const MAX_CSS_NUMBER: f64 = 1_000_000_000.0;

/// Parse the complete Runtime reply without importing capabilities or identity from page data.
pub(crate) fn parse_sample(
    reply: &Value,
    target: ActionTarget,
) -> Result<ActionState, &'static str> {
    if reply.get("error").is_some()
        || reply.get("exceptionDetails").is_some()
        || reply
            .get("wasThrown")
            .is_some_and(|value| value.as_bool() != Some(false))
    {
        return Err(UNAVAILABLE);
    }
    let result = reply
        .get("result")
        .and_then(Value::as_object)
        .ok_or(UNAVAILABLE)?;
    if result.get("type").and_then(Value::as_str) != Some("object")
        || result.get("subtype").is_some()
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
        return Err("dom_action_ref_stale");
    }
    let raw_bounds = sample
        .get("bounds")
        .and_then(Value::as_array)
        .filter(|bounds| bounds.len() == 4)
        .ok_or(UNAVAILABLE)?;
    let mut bounds = [0.0; 4];
    for (destination, value) in bounds.iter_mut().zip(raw_bounds) {
        *destination = value
            .as_f64()
            .filter(|number| number.is_finite() && number.abs() <= MAX_CSS_NUMBER)
            .ok_or(UNAVAILABLE)?;
    }
    if bounds[2] < 0.0
        || bounds[3] < 0.0
        || (bounds[0] + bounds[2]).abs() > MAX_CSS_NUMBER
        || (bounds[1] + bounds[3]).abs() > MAX_CSS_NUMBER
    {
        return Err(UNAVAILABLE);
    }
    let fact = |name: &str| -> Result<Option<bool>, &'static str> {
        match sample.get(name).ok_or(UNAVAILABLE)? {
            Value::Null => Ok(None),
            Value::Bool(value) => Ok(Some(*value)),
            _ => Err(UNAVAILABLE),
        }
    };
    Ok(ActionState {
        target,
        connected,
        visible: fact("visible")?,
        enabled: fact("enabled")?,
        editable: fact("editable")?,
        receives_pointer: fact("receives_pointer")?,
        bounds: Some(bounds),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target() -> ActionTarget {
        ActionTarget {
            frame_id: "main-frame".into(),
            loader_id: "loader-1".into(),
            backend_node_id: 42,
        }
    }
    fn reply() -> Value {
        json!({"result":{"type":"object","value":{
            "connected":true,"same_document":true,"visible":true,"enabled":true,
            "editable":false,"receives_pointer":true,"bounds":[-3.25,40.5,80.25,30.5]
        }}})
    }
    fn refuses(value: &Value, code: &'static str) {
        assert_eq!(parse_sample(value, target()).unwrap_err(), code);
    }

    #[test]
    fn fractional_css_boxes_and_the_attested_identity_are_preserved_without_conversion() {
        let parsed = parse_sample(&reply(), target()).unwrap();
        assert_eq!(parsed.target, target());
        assert!(parsed.connected);
        assert_eq!(parsed.bounds, Some([-3.25, 40.5, 80.25, 30.5]));
        assert_eq!(parsed.visible, Some(true));
        assert_eq!(parsed.enabled, Some(true));
        assert_eq!(parsed.editable, Some(false));
        assert_eq!(parsed.receives_pointer, Some(true));
    }

    #[test]
    fn explicit_unknown_facts_stay_unknown_instead_of_becoming_actionable() {
        let mut value = reply();
        for field in ["visible", "enabled", "editable", "receives_pointer"] {
            value["result"]["value"][field] = Value::Null;
        }
        let sample = parse_sample(&value, target()).unwrap();
        assert_eq!(sample.visible, None);
        assert_eq!(sample.enabled, None);
        assert_eq!(sample.editable, None);
        assert_eq!(sample.receives_pointer, None);
    }

    #[test]
    fn detached_or_foreign_nodes_refuse_even_without_remaining_state_fields() {
        for (connected, same_document) in [(false, true), (true, false), (false, false)] {
            refuses(
                &json!({"result":{"type":"object","value":{
                    "connected":connected,"same_document":same_document
                }}}),
                "dom_action_ref_stale",
            );
        }
    }

    #[test]
    fn exceptions_and_non_object_or_remote_only_results_are_not_actionability() {
        let mut exception = reply();
        let mut protocol_error = reply();
        protocol_error["error"] = json!({"code":-32000,"message":"context unavailable"});
        refuses(&protocol_error, "dom_action_state_unavailable");
        exception["exceptionDetails"] = json!({"text":"side effects unavailable"});
        let mut thrown = reply();
        thrown["wasThrown"] = json!(true);
        let mut wrong_type = reply();
        wrong_type["result"]["type"] = json!("string");
        let mut error = reply();
        error["result"]["subtype"] = json!("error");
        let mut unserializable = reply();
        unserializable["result"]["unserializableValue"] = json!("NaN");
        for value in [
            json!({}),
            json!({"result":{"objectId":"opaque"}}),
            exception,
            thrown,
            wrong_type,
            error,
            unserializable,
            json!({"result":{"type":"object","value":[]}}),
        ] {
            refuses(&value, "dom_action_state_unavailable");
        }
    }

    #[test]
    fn missing_and_coerced_boolean_facts_are_unavailable_not_false_or_true() {
        for field in [
            "connected",
            "same_document",
            "visible",
            "enabled",
            "editable",
            "receives_pointer",
        ] {
            let mut missing = reply();
            missing["result"]["value"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            refuses(&missing, "dom_action_state_unavailable");
            for invalid in [json!("true"), json!(0), json!([]), json!({})] {
                let mut value = reply();
                value["result"]["value"][field] = invalid;
                refuses(&value, "dom_action_state_unavailable");
            }
        }
    }

    #[test]
    fn invalid_bounds_or_extents_never_become_a_usable_box() {
        for bounds in [
            Value::Null,
            json!([]),
            json!([1, 2, 3]),
            json!([1, 2, 3, 4, 5]),
            json!([0, 0, -1, 2]),
            json!([0, 0, 2, -1]),
            json!([0, 0, "2", 3]),
            json!([1_000_000_001.0, 0, 1, 1]),
            json!([1_000_000_000.0, 0, 1, 1]),
            json!([0, 0, f64::INFINITY, 1]),
            json!([f64::NAN, 0, 1, 1]),
        ] {
            let mut value = reply();
            value["result"]["value"]["bounds"] = bounds;
            refuses(&value, "dom_action_state_unavailable");
        }
        let mut missing = reply();
        missing["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove("bounds");
        refuses(&missing, "dom_action_state_unavailable");
    }

    #[test]
    fn zero_area_is_valid_sample_data_not_invented_visibility_or_hit_test_success() {
        let mut value = reply();
        value["result"]["value"]["bounds"] = json!([1.5, 2.25, 0, 0]);
        value["result"]["value"]["visible"] = json!(false);
        value["result"]["value"]["receives_pointer"] = json!(false);
        let sample = parse_sample(&value, target()).unwrap();
        assert_eq!(sample.bounds, Some([1.5, 2.25, 0.0, 0.0]));
        assert_eq!(sample.visible, Some(false));
        assert_eq!(sample.receives_pointer, Some(false));
    }

    #[test]
    fn unknown_page_metadata_cannot_replace_the_callers_attested_target() {
        let mut value = reply();
        value["result"]["value"]["target"] = json!({"frame_id":"foreign","backend_node_id":99});
        value["result"]["value"]["native_pixel_mapping"] = json!("qualified");
        value["result"]["value"]["input_authority"] = json!(true);
        let sample = parse_sample(&value, target()).unwrap();
        assert_eq!(sample.target, target());
        assert_eq!(sample.bounds, Some([-3.25, 40.5, 80.25, 30.5]));
    }
}
