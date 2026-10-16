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
