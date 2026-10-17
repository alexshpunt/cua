//! Closed, text-free classifications for guarded CDP reads. Never publish exception prose.

use serde_json::Value;

const METHODS: [&str; 5] = [
    "Runtime.enable",
    "Page.createIsolatedWorld",
    "Runtime.callFunctionOn",
    "Runtime.releaseObjectGroup",
    "DOM.getNodeForLocation",
];
const CODES: [[&str; 3]; 5] = [
    [
        "dom_read_runtime_enable_malformed",
        "dom_read_runtime_enable_exception",
        "dom_read_runtime_enable_eval_error",
    ],
    [
        "dom_read_create_world_malformed",
        "dom_read_create_world_exception",
        "dom_read_create_world_eval_error",
    ],
    [
        "dom_read_call_function_malformed",
        "dom_read_call_function_exception",
        "dom_read_call_function_eval_error",
    ],
    [
        "dom_read_release_group_malformed",
        "dom_read_release_group_exception",
        "dom_read_release_group_eval_error",
    ],
    [
        "dom_read_hit_test_malformed",
        "dom_read_hit_test_exception",
        "dom_read_hit_test_eval_error",
    ],
];

/// Classify only known methods and the allowlisted EvalError class. EvalError alone is
/// not proof of a side-effect rejection; the exact browser still needs live qualification.
pub(super) fn check(method: &str, reply: &Value) -> Result<(), &'static str> {
    let index = METHODS
        .iter()
        .position(|known| *known == method)
        .ok_or("dom_read_unknown_method")?;
    if !reply.is_object()
        || reply.get("error").is_some()
        || reply.get("wasThrown").is_some_and(|v| !v.is_boolean())
    {
        return Err(CODES[index][0]);
    }
    if reply.get("exceptionDetails").is_some()
        || reply.get("wasThrown").and_then(Value::as_bool) == Some(true)
    {
        let eval_error = reply
            .pointer("/exceptionDetails/exception/className")
            .and_then(Value::as_str)
            == Some("EvalError");
        return Err(CODES[index][if eval_error { 2 } else { 1 }]);
    }
    Ok(())
}

/// Progress can retain only these exact authored codes, never a native message prefix.
pub(super) fn known_reason(message: &str) -> Option<&'static str> {
    CODES
        .iter()
        .flatten()
        .copied()
        .find(|known| *known == message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn method_exception_and_malformed_classifications_do_not_retain_private_prose() {
        for (index, method) in METHODS.iter().enumerate() {
            assert_eq!(check(method, &json!({})), Ok(()));
            assert_eq!(check(method, &json!([])), Err(CODES[index][0]));
            assert_eq!(
                check(method, &json!({"wasThrown":"yes"})),
                Err(CODES[index][0])
            );
            assert_eq!(
                check(
                    method,
                    &json!({"exceptionDetails":{"text":"PRIVATE","exception":{"className":"EvalError","description":"PRIVATE URL/node"}}})
                ),
                Err(CODES[index][2])
            );
            assert_eq!(
                check(
                    method,
                    &json!({"exceptionDetails":{"exception":{"className":"PRIVATE"}}})
                ),
                Err(CODES[index][1])
            );
            for code in CODES[index] {
                assert_eq!(known_reason(code), Some(code));
            }
        }
        assert_eq!(
            known_reason("dom_read_call_function_eval_error: PRIVATE"),
            None
        );
        assert_eq!(check("PRIVATE", &json!({})), Err("dom_read_unknown_method"));
    }
}
