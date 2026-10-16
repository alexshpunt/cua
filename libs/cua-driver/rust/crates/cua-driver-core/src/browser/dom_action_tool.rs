//! Public bounded DOM-action adapter; authorization stays at the canonical registry boundary.
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json,Value};
use crate::protocol::ToolResult;
use crate::tool::{ProtectedResourceOwnership,Tool,ToolDef};
use super::engine::BrowserEngine;
use super::refusal::{BrowserRefusal,BrowserRefusalCode};
use super::tools::{browser_resource_ownership,browser_protected_resource_scope};

/// Locator-grounded actions admitted through the same protected browser-input scope.
/// Its versioned receipt preserves bounded prefixes alongside a closed action projection;
/// it must not be narrowed to the legacy text-only action result and lose unknown effects.
pub struct BrowserDomActionTool { def:ToolDef, engine:Arc<BrowserEngine> }

impl BrowserDomActionTool {
    /// Construct this isolated candidate's explicit action surface for one shared engine.
    pub fn new(engine:Arc<BrowserEngine>)->Self {
        let def=ToolDef {
            name:"browser_dom_action".into(),
            description:"Use browser_dom_action to perform a bounded locator-grounded action in the current inspected page. Check the returned prefix and observed state before another action; dispatch is not application success.".into(),
            input_schema:json!({"type":"object","properties":{
                "session":{"type":"string","minLength":1,"maxLength":256,"description":"Use the explicit session that owns the bound browser."},
                "target_id":{"type":"string","minLength":1,"maxLength":256,"description":"Use the exact opaque browser binding from get_browser_state."},
                "tab_id":{"type":"string","minLength":1,"maxLength":256,"description":"Use the bound opaque tab id; this call never switches tabs."},
                "source_ref":{"type":"string","minLength":1,"maxLength":256,"description":"Use a current dom_inspect_v1 reference as document context. The locator resolves within that same document; the reference alone grants no input authority."},
                "input_route":{"type":"string","enum":["dom_event","trusted"],"description":"Select the route explicitly. dom_event uses programmatic DOM semantics, not trusted input, CSS hover or browser key defaults. trusted currently refuses; no fallback is automatic."},
                "request":request_schema()
            },"required":["session","target_id","tab_id","source_ref","input_route","request"],"additionalProperties":false}),
            read_only:false,destructive:false,idempotent:false,open_world:true,
        };
        Self {def,engine}
    }
}

fn request_schema()->Value {
    let kinds=[("click",None),("double_click",None),("right_click",None),("hover",Some("dwell_ms")),
        ("fill",Some("text")),("type",Some("text")),("press",Some("key")),("check",None),
        ("uncheck",None),("select_option",Some("values")),("focus",None),("scroll_into_view",None)];
    let alternatives=kinds.iter().map(|(action,parameter)| {
        let mut properties=serde_json::Map::new();
        properties.insert("action".into(),json!({"const":action}));
        for name in ["text","key","values","dwell_ms"] {
            if *parameter!=Some(name) {properties.insert(name.into(),Value::Bool(false));}
        }
        let required=parameter.map(|name|vec![name]).unwrap_or_default();
        json!({"properties":properties,"required":required})
    }).collect::<Vec<_>>();
    let locator= ["role","name","label","text","test_id","css"].into_iter()
        .map(|name|(name.to_owned(),json!({"type":"string","minLength":1,"maxLength":256})))
        .collect::<serde_json::Map<_,_>>();
    json!({"type":"object","properties":{
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
    fn def(&self)->&ToolDef {&self.def}
    async fn protected_resource_ownership(&self,adapter_id:&str,args:&Value)->ProtectedResourceOwnership {
        if adapter_id=="browser_bound_input" {browser_resource_ownership(&self.engine,args)}
        else {ProtectedResourceOwnership::UserOwned}
    }
    async fn protected_resource_scope(&self,adapter_id:&str,args:&Value)->Result<Option<Value>,String> {
        if adapter_id=="browser_bound_input" {browser_protected_resource_scope(&self.engine,args,"browser_dom_action").await}
        else {Ok(None)}
    }
    async fn invoke(&self,_args:Value)->ToolResult {
        BrowserRefusal::new(BrowserRefusalCode::BrowserActionUnavailable,
            "dom_action_public_adapter_unavailable").to_tool_result()
    }
}
