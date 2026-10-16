//! Public bounded DOM-action adapter; authorization stays at the canonical registry boundary.
use super::dom_action_progress::DomActionProgress;
use super::dom_action_request::{parse_request, DomActionRequest};
use super::engine::BrowserEngine;
use super::engine::DomActionRoute;
use super::refusal::{BrowserRefusal, BrowserRefusalCode};
use super::tools::{browser_protected_resource_scope, browser_resource_ownership};
use crate::action_record::{
    ActionEffect, ActionExecutionRecord, ActionRefusal, ActionTransport, ActualDelivery,
    RequestedDelivery,
};
use crate::protocol::ToolResult;
use crate::tool::{ProtectedResourceOwnership, Tool, ToolDef};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Locator-grounded actions admitted through the same protected browser-input scope.
/// Its versioned receipt preserves bounded prefixes alongside a closed action projection;
/// it must not be narrowed to the legacy text-only action result and lose unknown effects.
pub struct BrowserDomActionTool {
    def: ToolDef,
    engine: Arc<BrowserEngine>,
}

impl BrowserDomActionTool {
    /// Construct this isolated candidate's explicit action surface for one shared engine.
    pub fn new(engine: Arc<BrowserEngine>) -> Self {
        let mut session = crate::tool_schema::session_schema_with(
            "Use the explicit session that owns the bound browser.",
        );
        session["minLength"] = json!(1);
        session["maxLength"] = json!(256);
        let def=ToolDef {
            name:"browser_dom_action".into(),
            description:"Use browser_dom_action to perform a bounded locator-grounded action in the current inspected page. Check the returned prefix and observed state before another action; dispatch is not application success.".into(),
            input_schema:json!({"type":"object","properties":{
                "session":session,
                "target_id":{"type":"string","minLength":1,"maxLength":256,"description":"Use the exact opaque browser binding from get_browser_state."},
                "tab_id":{"type":"string","minLength":1,"maxLength":256,"description":"Use the bound opaque tab id; this call never switches tabs."},
                "source_ref":{"type":"string","minLength":1,"maxLength":256,"description":"Use a current dom_inspect_v1 reference as document context. The locator resolves within that same document; the reference alone grants no input authority."},
                "input_route":{"type":"string","enum":["dom_event","trusted"],"description":"Select the route explicitly. dom_event uses programmatic DOM semantics, not trusted input, CSS hover or browser key defaults. trusted currently refuses; no fallback is automatic."},
                "request":request_schema()
            },"required":["session","target_id","tab_id","source_ref","input_route","request"],"additionalProperties":false}),
            read_only:false,destructive:false,idempotent:false,open_world:true,
        };
        Self { def, engine }
    }
}

struct WireRequest {
    session: String,
    target: String,
    tab: String,
    source: String,
    route: DomActionRoute,
    request: DomActionRequest,
}
fn parse_wire(args: &Value) -> Result<WireRequest, &'static str> {
    const INVALID: &str = "dom_action_invalid_request";
    let mut public = args.clone();
    // The canonical registry already sanitized caller metadata before injecting trusted fields.
    // This parser ignores those transport-only fields; it never reads authority from them.
    crate::tool_args::sanitize_reserved_args(&mut public);
    let raw = public.as_object().ok_or(INVALID)?;
    if serde_json::to_vec(&public).map_err(|_| INVALID)?.len() > 32 * 1024
        || raw.keys().any(|key| {
            !matches!(
                key.as_str(),
                "session" | "target_id" | "tab_id" | "source_ref" | "input_route" | "request"
            )
        })
    {
        return Err(INVALID);
    }
    let string = |key: &str, limit: usize| -> Result<String, &'static str> {
        raw.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.chars().count() <= limit)
            .map(str::to_owned)
            .ok_or(INVALID)
    };
    let session = string("session", 512)?;
    let prefix = crate::tool::current_dispatch_authorization_context()
        .map(|context| format!("__cua_runtime_{}:", context.runtime_scope_key()));
    let public_session = prefix
        .as_deref()
        .and_then(|prefix| session.strip_prefix(prefix))
        .unwrap_or(&session);
    if public_session.is_empty()
        || public_session == "default"
        || public_session.chars().count() > 256
    {
        return Err(INVALID);
    }
    let route = match raw.get("input_route").and_then(Value::as_str) {
        Some("dom_event") => DomActionRoute::DomEvent,
        Some("trusted") => DomActionRoute::Trusted,
        _ => return Err(INVALID),
    };
    Ok(WireRequest {
        session,
        target: string("target_id", 256)?,
        tab: string("tab_id", 256)?,
        source: string("source_ref", 256)?,
        route,
        request: parse_request(raw.get("request").ok_or(INVALID)?)?,
    })
}

fn receipt(
    route: Option<&str>,
    progress: DomActionProgress,
    native: Result<Value, BrowserRefusal>,
) -> ToolResult {
    let prefix = progress.snapshot();
    let attempted = prefix["mutation_attempted"] == true;
    let error = native.as_ref().err();
    let message = error.map(|error| {
        let mut remaining = 512;
        error
            .message
            .chars()
            .take_while(|character| {
                if character.len_utf8() > remaining {
                    return false;
                }
                remaining -= character.len_utf8();
                true
            })
            .collect::<String>()
    });
    let mut record = ActionExecutionRecord::new(
        if attempted {
            ActionEffect::Unverifiable
        } else {
            ActionEffect::Refused
        },
        ActionTransport::BrowserCdpRuntimeFunction,
        RequestedDelivery::NotApplicable,
    );
    if attempted {
        record.actual_delivery = Some(ActualDelivery::Unknown);
    } else if let Some(error) = error {
        record.refusal = Some(ActionRefusal {
            code: error.code.as_str().to_owned(),
            hint: message.clone(),
        });
    }
    let action_result=match record.public_result() {
        Ok(value)=>value,
        Err(_)=>return ToolResult::error("DOM action accounting is unavailable. An effect may have occurred; inspect state before any retry."),
    };
    let refusal = error.map(|error| {
        json!({"code":error.code.as_str(),
        "message":message})
    });
    let status = if native.is_ok() {
        "dispatched"
    } else {
        "refused"
    };
    let output = json!({
        "format":"dom_action_v1","status":status,"input_route":route,
        "trusted":false,"atomic":false,"native_pixel_mapping":"unavailable",
        "action_result":action_result,"progress":prefix,
        "observed":native.as_ref().ok().and_then(|value|value.get("observed"))
            .cloned().unwrap_or_else(||json!([])),
        "refusal":refusal,"limitations":["programmatic_dom_semantics_only","no_css_hover_proof",
            "no_trusted_key_defaults","no_application_success_proof"]
    });
    let reason = refusal
        .as_ref()
        .map(|error| {
            format!(
                " ({}): {}",
                error["code"].as_str().unwrap_or("unavailable"),
                error["message"].as_str().unwrap_or("outcome unavailable")
            )
        })
        .unwrap_or_default();
    let text=format!("DOM action {status}{reason}; attempted {}, acknowledged {}. Effects unverified; no automatic retry. Read fresh state.",
        prefix["attempted_steps"].as_array().map_or(0,Vec::len),
        prefix["acknowledged_steps"].as_array().map_or(0,Vec::len));
    let mut result = ToolResult::text(text).with_structured(output);
    result.action_record = Some(record);
    result
}
fn request_schema() -> Value {
    let kinds = [
        ("click", None),
        ("double_click", None),
        ("right_click", None),
        ("hover", Some("dwell_ms")),
        ("fill", Some("text")),
        ("type", Some("text")),
        ("press", Some("key")),
        ("check", None),
        ("uncheck", None),
        ("select_option", Some("values")),
        ("focus", None),
        ("scroll_into_view", None),
    ];
    let alternatives = kinds
        .iter()
        .map(|(action, parameter)| {
            let mut properties = serde_json::Map::new();
            properties.insert("action".into(), json!({"const":action}));
            for name in ["text", "key", "values", "dwell_ms"] {
                if *parameter != Some(name) {
                    properties.insert(name.into(), Value::Bool(false));
                }
            }
            let required = parameter.map(|name| vec![name]).unwrap_or_default();
            json!({"properties":properties,"required":required})
        })
        .collect::<Vec<_>>();
    let locator = ["role", "name", "label", "text", "test_id", "css"]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                json!({"type":"string","minLength":1,"maxLength":256}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({"type":"object","description":"Use one bounded locator action with only its action-specific parameters.","properties":{
        "action":{"type":"string","enum":kinds.iter().map(|(name,_)|*name).collect::<Vec<_>>()},
        "locator":{"type":"object","properties":locator,"minProperties":1,"additionalProperties":false,
            "description":"Combine exact predicates with AND. A duplicate or incomplete match refuses; no convenient first match is chosen."},
        "timeout_ms":{"type":"integer","minimum":1,"maximum":10000,"default":5000,
            "description":"Total request deadline in milliseconds, including bounded readiness and dwell."},
        "text":{"type":"string","maxLength":2000,"description":"Text for fill or type only; an empty string is allowed."},
        "key":{"type":"string","minLength":1,"maxLength":64,"description":"Closed key chord for press only, such as Enter or Control+A. Layout-dependent unsupported chords refuse."},
        "values":{"type":"array","minItems":1,"maxItems":32,"uniqueItems":true,"items":{"type":"string","maxLength":256},
            "description":"Exact option values for select_option only. Each value must identify one enabled option."},
        "dwell_ms":{"type":"integer","minimum":1,"maximum":10000,"description":"Required duration for hover only; it remains inside timeout_ms."}
    },"required":["action","locator"],"additionalProperties":false,"oneOf":alternatives})
}

#[async_trait]
impl Tool for BrowserDomActionTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    async fn protected_resource_ownership(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> ProtectedResourceOwnership {
        if adapter_id == "browser_bound_input" {
            browser_resource_ownership(&self.engine, args)
        } else {
            ProtectedResourceOwnership::UserOwned
        }
    }
    async fn protected_resource_scope(
        &self,
        adapter_id: &str,
        args: &Value,
    ) -> Result<Option<Value>, String> {
        if adapter_id == "browser_bound_input" {
            browser_protected_resource_scope(&self.engine, args, "browser_dom_action").await
        } else {
            Ok(None)
        }
    }
    async fn invoke(&self, args: Value) -> ToolResult {
        let progress = DomActionProgress::default();
        let wire = match parse_wire(&args) {
            Ok(wire) => wire,
            Err(reason) => {
                progress.finish("refused", Some(reason));
                return receipt(
                    None,
                    progress,
                    Err(BrowserRefusal::new(
                        BrowserRefusalCode::BrowserActionUnavailable,
                        reason,
                    )),
                );
            }
        };
        let route = Some(match wire.route {
            DomActionRoute::Trusted => "trusted",
            DomActionRoute::DomEvent => "dom_event",
        });
        let native = self
            .engine
            .dispatch_dom_action(
                &wire.session,
                &wire.target,
                &wire.tab,
                &wire.source,
                &wire.request,
                wire.route,
                progress.clone(),
            )
            .await;
        receipt(route, progress, native)
    }
}
