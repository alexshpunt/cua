//! Closed requests for locator-grounded actions. Parsing never resolves or ranks a target.

use super::dom_inspection::DomLocator;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DomActionKind {
    Click,
    DoubleClick,
    RightClick,
    Hover,
    Fill,
    Type,
    Press,
    Check,
    Uncheck,
    SelectOption,
    Focus,
    ScrollIntoView,
}

/// Validated request data, not input authority or proof of a live unique match.
#[derive(Debug, Clone)]
pub(crate) struct DomActionRequest {
    pub(crate) kind: DomActionKind,
    pub(crate) locator: DomLocator,
    pub(crate) timeout_ms: u64,
    pub(crate) text: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) values: Vec<String>,
    pub(crate) dwell_ms: Option<u64>,
}

const INVALID_REQUEST: &str = "dom_action_invalid_request";

/// Reject malformed or irrelevant arguments before native lookup, waiting or side effects.
pub(crate) fn parse_request(value: &Value) -> Result<DomActionRequest, &'static str> {
    let raw = value.as_object().ok_or(INVALID_REQUEST)?;
    if raw.keys().any(|key| {
        !matches!(
            key.as_str(),
            "action" | "locator" | "timeout_ms" | "text" | "key" | "values" | "dwell_ms"
        )
    }) {
        return Err(INVALID_REQUEST);
    }
    let kind = match raw.get("action").and_then(Value::as_str) {
        Some("click") => DomActionKind::Click,
        Some("double_click") => DomActionKind::DoubleClick,
        Some("right_click") => DomActionKind::RightClick,
        Some("hover") => DomActionKind::Hover,
        Some("fill") => DomActionKind::Fill,
        Some("type") => DomActionKind::Type,
        Some("press") => DomActionKind::Press,
        Some("check") => DomActionKind::Check,
        Some("uncheck") => DomActionKind::Uncheck,
        Some("select_option") => DomActionKind::SelectOption,
        Some("focus") => DomActionKind::Focus,
        Some("scroll_into_view") => DomActionKind::ScrollIntoView,
        _ => return Err(INVALID_REQUEST),
    };
    let fields = raw
        .get("locator")
        .and_then(Value::as_object)
        .ok_or(INVALID_REQUEST)?;
    if fields.is_empty() {
        return Err(INVALID_REQUEST);
    }
    let mut locator = DomLocator::default();
    for (key, value) in fields {
        let destination = match key.as_str() {
            "role" => &mut locator.role,
            "name" => &mut locator.name,
            "label" => &mut locator.label,
            "text" => &mut locator.text,
            "test_id" => &mut locator.test_id,
            "css" => &mut locator.css,
            _ => return Err(INVALID_REQUEST),
        };
        *destination = Some(limited_string(value, 256, false)?.to_owned());
    }
    let timeout_ms = raw
        .get("timeout_ms")
        .map(milliseconds)
        .transpose()?
        .unwrap_or(5_000);
    let parameter = match kind {
        DomActionKind::Fill | DomActionKind::Type => Some("text"),
        DomActionKind::Press => Some("key"),
        DomActionKind::SelectOption => Some("values"),
        DomActionKind::Hover => Some("dwell_ms"),
        _ => None,
    };
    for field in ["text", "key", "values", "dwell_ms"] {
        if raw.contains_key(field) && parameter != Some(field) {
            return Err(INVALID_REQUEST);
        }
    }
    let mut request = DomActionRequest {
        kind,
        locator,
        timeout_ms,
        text: None,
        key: None,
        values: Vec::new(),
        dwell_ms: None,
    };
    match kind {
        DomActionKind::Fill | DomActionKind::Type => {
            request.text = Some(
                limited_string(raw.get("text").ok_or(INVALID_REQUEST)?, 2_000, true)?.to_owned(),
            );
        }
        DomActionKind::Press => {
            let key = limited_string(raw.get("key").ok_or(INVALID_REQUEST)?, 64, false)?;
            if key.chars().any(char::is_control) {
                return Err(INVALID_REQUEST);
            }
            request.key = Some(key.to_owned());
        }
        DomActionKind::SelectOption => {
            let values = raw
                .get("values")
                .and_then(Value::as_array)
                .ok_or(INVALID_REQUEST)?;
            if values.is_empty() || values.len() > 32 {
                return Err(INVALID_REQUEST);
            }
            let mut unique = std::collections::HashSet::new();
            for value in values {
                let value = limited_string(value, 256, true)?;
                if !unique.insert(value) {
                    return Err(INVALID_REQUEST);
                }
                request.values.push(value.to_owned());
            }
        }
        DomActionKind::Hover => {
            request.dwell_ms = Some(milliseconds(raw.get("dwell_ms").ok_or(INVALID_REQUEST)?)?);
        }
        _ => {}
    }
    Ok(request)
}

fn limited_string(value: &Value, max: usize, empty_allowed: bool) -> Result<&str, &'static str> {
    let text = value.as_str().ok_or(INVALID_REQUEST)?;
    if (!empty_allowed && text.is_empty()) || text.chars().take(max + 1).count() > max {
        return Err(INVALID_REQUEST);
    }
    Ok(text)
}

fn milliseconds(value: &Value) -> Result<u64, &'static str> {
    value
        .as_u64()
        .filter(|number| (1..=10_000).contains(number))
        .ok_or(INVALID_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const INVALID: &str = "dom_action_invalid_request";

    fn click() -> Value {
        json!({"action":"click","locator":{"role":"button","name":"Save"}})
    }

    fn refuses(value: Value) {
        assert_eq!(parse_request(&value).unwrap_err(), INVALID, "{value}");
    }

    #[test]
    fn each_action_has_only_its_own_explicit_parameters() {
        for (action, kind, params) in [
            ("click", DomActionKind::Click, json!({})),
            ("double_click", DomActionKind::DoubleClick, json!({})),
            ("right_click", DomActionKind::RightClick, json!({})),
            ("hover", DomActionKind::Hover, json!({"dwell_ms":10})),
            ("fill", DomActionKind::Fill, json!({"text":""})),
            ("type", DomActionKind::Type, json!({"text":"two words\n🙂"})),
            ("press", DomActionKind::Press, json!({"key":"Control+A"})),
            ("check", DomActionKind::Check, json!({})),
            ("uncheck", DomActionKind::Uncheck, json!({})),
            (
                "select_option",
                DomActionKind::SelectOption,
                json!({"values":["","second"]}),
            ),
            ("focus", DomActionKind::Focus, json!({})),
            ("scroll_into_view", DomActionKind::ScrollIntoView, json!({})),
        ] {
            let mut input = click();
            input["action"] = json!(action);
            input
                .as_object_mut()
                .unwrap()
                .extend(params.as_object().unwrap().clone());
            let parsed = parse_request(&input).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.timeout_ms, 5_000);
            assert_eq!(
                parsed.text.as_deref(),
                params.get("text").and_then(Value::as_str)
            );
            assert_eq!(
                parsed.key.as_deref(),
                params.get("key").and_then(Value::as_str)
            );
            assert_eq!(
                parsed.dwell_ms,
                params.get("dwell_ms").and_then(Value::as_u64)
            );
            assert_eq!(
                parsed.values,
                if action == "select_option" {
                    vec!["", "second"]
                } else {
                    vec![]
                }
            );
        }
    }

    #[test]
    fn missing_unknown_and_wrongly_typed_fields_are_not_coerced() {
        for input in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"locator":{"css":"button"}}),
            json!({"action":"click"}),
            json!({"action":null,"locator":{"css":"button"}}),
            json!({"action":"CLICK","locator":{"css":"button"}}),
            json!({"action":"click","locator":[]}),
            json!({"action":"click","locator":null}),
        ] {
            refuses(input);
        }
        for field in [
            "ref",
            "x",
            "y",
            "input_route",
            "fallback",
            "script",
            "delivery_mode",
            "force",
            "scope_ref",
        ] {
            let mut input = click();
            input[field] = json!("backdoor");
            refuses(input);
        }
    }

    #[test]
    fn locator_predicates_are_retained_together_without_matching_or_ranking() {
        let locator = json!({"role":"button","name":" Save ","label":"Duplicate","text":"substring",
            "test_id":"save-button","css":"button[data-ready]"});
        let parsed = parse_request(&json!({"action":"click","locator":locator})).unwrap();
        assert_eq!(parsed.locator.role.as_deref(), Some("button"));
        assert_eq!(parsed.locator.name.as_deref(), Some(" Save "));
        assert_eq!(parsed.locator.label.as_deref(), Some("Duplicate"));
        assert_eq!(parsed.locator.text.as_deref(), Some("substring"));
        assert_eq!(parsed.locator.test_id.as_deref(), Some("save-button"));
        assert_eq!(parsed.locator.css.as_deref(), Some("button[data-ready]"));
        for locator in [
            json!({}),
            json!({"css":""}),
            json!({"role":null}),
            json!({"role":true}),
            json!({"name":"Save","ordinal":0}),
            json!({"css":"button","name":null}),
        ] {
            refuses(json!({"action":"click","locator":locator}));
        }
    }

    #[test]
    fn unicode_budgets_count_scalars_and_empty_fill_is_a_valid_clear_request() {
        let exact_locator = "🙂".repeat(256);
        let parsed =
            parse_request(&json!({"action":"fill","locator":{"label":exact_locator},"text":""}))
                .unwrap();
        assert_eq!(parsed.text.as_deref(), Some(""));
        assert_eq!(parsed.locator.label.unwrap().chars().count(), 256);
        refuses(json!({"action":"click","locator":{"text":"🙂".repeat(257)}}));
        let parsed = parse_request(
            &json!({"action":"type","locator":{"test_id":"edit"},"text":"🙂".repeat(2_000)}),
        )
        .unwrap();
        assert_eq!(parsed.text.unwrap().chars().count(), 2_000);
        refuses(json!({"action":"fill","locator":{"test_id":"edit"},"text":"🙂".repeat(2_001)}));
    }

    #[test]
    fn timeout_and_hover_dwell_are_finite_integer_budgets_not_wait_forever_switches() {
        for value in [json!(1), json!(10_000)] {
            let mut input = click();
            input["timeout_ms"] = value.clone();
            assert_eq!(
                parse_request(&input).unwrap().timeout_ms,
                value.as_u64().unwrap()
            );
            input["action"] = json!("hover");
            input["dwell_ms"] = value.clone();
            assert_eq!(parse_request(&input).unwrap().dwell_ms, value.as_u64());
        }
        for bad in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!(10_001),
            json!("100"),
            json!(true),
        ] {
            let mut input = click();
            input["timeout_ms"] = bad.clone();
            refuses(input);
            refuses(json!({"action":"hover","locator":{"css":"button"},"dwell_ms":bad}));
        }
        refuses(json!({"action":"hover","locator":{"css":"button"}}));
    }

    #[test]
    fn irrelevant_missing_or_null_action_parameters_are_rejected() {
        for (action, required) in [
            ("fill", "text"),
            ("type", "text"),
            ("press", "key"),
            ("select_option", "values"),
            ("hover", "dwell_ms"),
        ] {
            let mut input = click();
            input["action"] = json!(action);
            refuses(input.clone());
            input[required] = Value::Null;
            refuses(input);
        }
        for (action, own) in [
            ("click", json!({})),
            ("fill", json!({"text":"value"})),
            ("press", json!({"key":"Enter"})),
            ("select_option", json!({"values":["one"]})),
            ("hover", json!({"dwell_ms":5})),
        ] {
            for param in ["text", "key", "values", "dwell_ms"] {
                if own.get(param).is_some() {
                    continue;
                }
                let mut input = click();
                input["action"] = json!(action);
                input
                    .as_object_mut()
                    .unwrap()
                    .extend(own.as_object().unwrap().clone());
                input[param] = Value::Null;
                refuses(input);
            }
        }
    }

    #[test]
    fn press_keys_are_bounded_data_not_control_characters_or_implicit_key_mapping() {
        for key in ["Enter".to_owned(), "🙂".repeat(64)] {
            let parsed =
                parse_request(&json!({"action":"press","locator":{"css":"input"},"key":key}))
                    .unwrap();
            assert_eq!(parsed.key.as_deref(), Some(key.as_str()));
        }
        for key in [
            "".to_owned(),
            "x".repeat(65),
            "Enter\n".into(),
            "\0".into(),
            "\u{7f}".into(),
            "\u{85}".into(),
        ] {
            refuses(json!({"action":"press","locator":{"css":"input"},"key":key}));
        }
        refuses(json!({"action":"press","locator":{"css":"input"},"key":3}));
    }

    #[test]
    fn select_values_are_explicit_unique_bounded_strings_in_original_order() {
        let values = (0..32)
            .map(|index| format!("{index}-{}", "🙂".repeat(250)))
            .collect::<Vec<_>>();
        let parsed = parse_request(
            &json!({"action":"select_option","locator":{"label":"Choice"},"values":values}),
        )
        .unwrap();
        assert_eq!(parsed.values, values);
        let exact = "🙂".repeat(256);
        assert_eq!(
            parse_request(
                &json!({"action":"select_option","locator":{"css":"select"},"values":[exact]})
            )
            .unwrap()
            .values[0],
            exact
        );
        for values in [
            json!([]),
            json!(["same", "same"]),
            json!([null]),
            json!([3]),
            json!("one"),
            json!(["🙂".repeat(257)]),
            json!((0..33).map(|n| n.to_string()).collect::<Vec<_>>()),
        ] {
            refuses(json!({"action":"select_option","locator":{"css":"select"},"values":values}));
        }
    }
}
