//! Closed programmatic DOM operations, never trusted input or coordinate substitution.
//!
//! The caller must explicitly select the DOM-event route, attest the isolated world and node,
//! revalidate between steps, retain attempted prefixes, and stop on an unknown acknowledgement.
//! Synthetic hover events do not establish CSS `:hover`; synthetic keys do not insert text or
//! perform trusted browser defaults. A dispatched event is not application-state success.

use super::dom_action_key::DomPressKey;
use super::dom_action_request::{DomActionKind, DomActionRequest};
use serde_json::{json, Value};

/// One acknowledged boundary. A dropped call may still finish this one operation, never the plan.
#[derive(Debug, Clone)]
pub(crate) struct DomActionStep {
    pub(crate) name: &'static str,
    pub(crate) argument: Value,
}

/// Build only fixed operations. This plan neither authorizes input nor resolves a target.
pub(crate) fn plan(
    request: &DomActionRequest,
    key: Option<&DomPressKey>,
) -> Result<Vec<DomActionStep>, &'static str> {
    let step = |name, argument| DomActionStep { name, argument };
    let plain = |name| step(name, json!({}));
    use DomActionKind::*;
    Ok(match request.kind {
        Click => vec![plain("click")],
        DoubleClick => vec![plain("click"), plain("click"), plain("dblclick")],
        RightClick => vec![plain("contextmenu")],
        Hover => vec![
            plain("pointerover"),
            plain("mouseover"),
            plain("pointerenter"),
            plain("mouseenter"),
            step(
                "dwell",
                json!(request.dwell_ms.ok_or("dom_action_invalid_request")?),
            ),
        ],
        Focus => vec![plain("focus")],
        ScrollIntoView => vec![plain("scroll_into_view")],
        Check | Uncheck => vec![step(
            "set_checked",
            json!({"checked":request.kind == Check}),
        )],
        Fill | Type => {
            let text = request
                .text
                .as_deref()
                .ok_or("dom_action_invalid_request")?;
            let input_type = if request.kind == Fill {
                "insertReplacementText"
            } else {
                "insertText"
            };
            vec![
                plain("focus"),
                step("beforeinput", json!({"text":text,"input_type":input_type})),
                step(
                    if request.kind == Fill {
                        "fill"
                    } else {
                        "insert_text"
                    },
                    json!({"text":text}),
                ),
                step("input", json!({"text":text,"input_type":input_type})),
            ]
        }
        Press => {
            let key = key.ok_or("dom_action_key_unavailable")?;
            let args = json!({"key":key.key,"code":key.code,"modifiers":key.modifiers});
            vec![
                plain("focus"),
                step("keydown", args.clone()),
                step("keyup", args),
            ]
        }
        SelectOption => vec![
            step("select_values", json!({"values":request.values})),
            step("input", json!({"text":null,"input_type":null})),
            plain("change"),
        ],
    })
}

/// Check supported control semantics before any planned focus, setter or event dispatch.
pub(crate) const PREFLIGHT_FUNCTION: &str = r#"function(node, request) {
  const get = (prototype, name, value) => Object.getOwnPropertyDescriptor(prototype, name).get.call(value);
  const fail = reason => ({ok:false,reason});
  if (!get(Node.prototype,'isConnected',node) || get(Node.prototype,'ownerDocument',node) !== document)
    return fail('dom_action_ref_stale');
  const textControl = () => {
    if (node instanceof HTMLTextAreaElement) return true;
    if (!(node instanceof HTMLInputElement)) return false;
    return ['text','search','tel','url','password'].includes(get(HTMLInputElement.prototype,'type',node));
  };
  if (!request || typeof request !== 'object') return fail('dom_action_operation_unavailable');
  switch(request.action) {
    case 'fill': case 'type':
      if (!textControl() || typeof request.text !== 'string' || Array.from(request.text).length > 2000)
        return fail('dom_action_control_unavailable');
      return {ok:true};
    case 'check': case 'uncheck':
      return node instanceof HTMLInputElement && get(HTMLInputElement.prototype,'type',node) === 'checkbox'
        ? {ok:true} : fail('dom_action_control_unavailable');
    case 'select_option': {
      if (!(node instanceof HTMLSelectElement) || !Array.isArray(request.values) || !request.values.length || request.values.length > 32)
        return fail('dom_action_control_unavailable');
      if (request.values.some(value => typeof value !== 'string' || Array.from(value).length > 256) || new Set(request.values).size !== request.values.length)
        return fail('dom_action_control_unavailable');
      if (!get(HTMLSelectElement.prototype,'multiple',node) && request.values.length !== 1)
        return fail('dom_action_control_unavailable');
      const options = get(HTMLSelectElement.prototype,'options',node);
      const count = get(HTMLCollection.prototype,'length',options);
      if (!Number.isInteger(count) || count > 200) return fail('dom_action_option_unavailable');
      for (const requested of request.values) {
        let matches = 0;
        for (let i=0;i<count;i++) {
          const option = HTMLCollection.prototype.item.call(options,i);
          if (get(HTMLOptionElement.prototype,'value',option) !== requested) continue;
          matches++;
          const parent = get(Node.prototype,'parentNode',option);
          if (get(HTMLOptionElement.prototype,'disabled',option) ||
              (parent instanceof HTMLOptGroupElement && get(HTMLOptGroupElement.prototype,'disabled',parent)))
            return fail('dom_action_option_unavailable');
        }
        if (matches !== 1) return fail('dom_action_option_unavailable');
      }
      return {ok:true};
    }
    case 'click': case 'double_click':
    case 'focus': case 'press':
      return node instanceof HTMLElement ? {ok:true} : fail('dom_action_control_unavailable');
    case 'right_click': case 'hover': case 'scroll_into_view':
      return node instanceof Element ? {ok:true} : fail('dom_action_control_unavailable');
    default: return fail('dom_action_operation_unavailable');
  }
}"#;

/// Execute one fixed operation in the attested isolated world. Exceptions remain exceptions.
pub(crate) const STEP_FUNCTION: &str = r#"function(node, step) {
  const get = (prototype, name, value) => Object.getOwnPropertyDescriptor(prototype, name).get.call(value);
  const set = (prototype, name, value, next) => Object.getOwnPropertyDescriptor(prototype, name).set.call(value,next);
  const fail = reason => ({ok:false,reason});
  const ok = observed => ({ok:true,observed});
  if (!get(Node.prototype,'isConnected',node) || get(Node.prototype,'ownerDocument',node) !== document)
    return fail('dom_action_ref_stale');
  if (!step || typeof step !== 'object' || !step.argument || typeof step.argument !== 'object')
    return fail('dom_action_operation_unavailable');
  const args = step.argument;
  const dispatch = event => EventTarget.prototype.dispatchEvent.call(node,event);
  const textPrototype = () => {
    if (node instanceof HTMLTextAreaElement) return HTMLTextAreaElement.prototype;
    if (node instanceof HTMLInputElement && ['text','search','tel','url','password'].includes(get(HTMLInputElement.prototype,'type',node)))
      return HTMLInputElement.prototype;
    return null;
  };
  const focusMatches = () => {
    const root = Node.prototype.getRootNode.call(node);
    return root instanceof ShadowRoot ? get(ShadowRoot.prototype,'activeElement',root) === node
      : root === document && get(Document.prototype,'activeElement',document) === node;
  };
  switch(step.name) {
    case 'click':
      if (!(node instanceof HTMLElement)) return fail('dom_action_control_unavailable');
      HTMLElement.prototype.click.call(node); return ok({activation_invoked:true});
    case 'focus':
      if (!(node instanceof HTMLElement)) return fail('dom_action_control_unavailable');
      HTMLElement.prototype.focus.call(node,{preventScroll:true});
      return focusMatches() ? ok({focused:true}) : fail('dom_action_not_focused');
    case 'scroll_into_view':
      Element.prototype.scrollIntoView.call(node,{block:'nearest',inline:'nearest',behavior:'instant'});
      return ok({scroll_requested:true});
    case 'set_checked': {
      if (!(node instanceof HTMLInputElement) || get(HTMLInputElement.prototype,'type',node) !== 'checkbox' || typeof args.checked !== 'boolean')
        return fail('dom_action_control_unavailable');
      if (get(HTMLInputElement.prototype,'checked',node) === args.checked) return ok({already_matched:true,checked_matches:true});
      HTMLElement.prototype.click.call(node);
      return get(HTMLInputElement.prototype,'checked',node) === args.checked ? ok({checked_matches:true}) : fail('dom_action_postcondition_unconfirmed');
    }
    case 'fill': case 'insert_text': {
      const prototype = textPrototype();
      if (!prototype || typeof args.text !== 'string' || Array.from(args.text).length > 2000)
        return fail('dom_action_control_unavailable');
      if (step.name === 'fill') {
        set(prototype,'value',node,args.text);
        return get(prototype,'value',node) === args.text ? ok({value_matches:true}) : fail('dom_action_postcondition_unconfirmed');
      }
      if (!focusMatches()) return fail('dom_action_not_focused');
      const start = get(prototype,'selectionStart',node), end = get(prototype,'selectionEnd',node);
      if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end < start) return fail('dom_action_control_unavailable');
      prototype.setRangeText.call(node,args.text,start,end,'end'); return ok({text_applied:true});
    }
    case 'beforeinput': case 'input': {
      if (args.text === null && args.input_type === null && step.name === 'input')
        return ok({event_cancelled:!dispatch(new Event('input',{bubbles:true,composed:true}))});
      if (typeof args.text !== 'string' || Array.from(args.text).length > 2000 || !['insertText','insertReplacementText'].includes(args.input_type))
        return fail('dom_action_control_unavailable');
      const accepted = dispatch(new InputEvent(step.name,{bubbles:true,composed:true,cancelable:step.name === 'beforeinput',data:args.text,inputType:args.input_type}));
      if (step.name === 'beforeinput' && !accepted) return fail('dom_action_event_cancelled');
      return ok({event_cancelled:!accepted});
    }
    case 'select_values': {
      if (!(node instanceof HTMLSelectElement) || !Array.isArray(args.values) || !args.values.length || args.values.length > 32 ||
          args.values.some(value => typeof value !== 'string' || Array.from(value).length > 256) || new Set(args.values).size !== args.values.length)
        return fail('dom_action_control_unavailable');
      if (!get(HTMLSelectElement.prototype,'multiple',node) && args.values.length !== 1) return fail('dom_action_control_unavailable');
      const collection = get(HTMLSelectElement.prototype,'options',node), count = get(HTMLCollection.prototype,'length',collection);
      if (!Number.isInteger(count) || count > 200) return fail('dom_action_option_unavailable');
      const options = [], values = [];
      for (let i=0;i<count;i++) { const option=HTMLCollection.prototype.item.call(collection,i); options.push(option); values.push(get(HTMLOptionElement.prototype,'value',option)); }
      for (const requested of args.values) {
        const matches=[]; for(let i=0;i<count;i++) if(values[i]===requested) matches.push(options[i]);
        if(matches.length!==1) return fail('dom_action_option_unavailable');
        const option=matches[0], parent=get(Node.prototype,'parentNode',option);
        if(get(HTMLOptionElement.prototype,'disabled',option) || (parent instanceof HTMLOptGroupElement && get(HTMLOptGroupElement.prototype,'disabled',parent)))
          return fail('dom_action_option_unavailable');
      }
      for(let i=0;i<count;i++) set(HTMLOptionElement.prototype,'selected',options[i],args.values.includes(values[i]));
      const matches = options.every((option,i) => get(HTMLOptionElement.prototype,'selected',option) === args.values.includes(values[i]));
      return matches ? ok({selected_matches:true}) : fail('dom_action_postcondition_unconfirmed');
    }
    case 'keydown': case 'keyup': {
      if (typeof args.key !== 'string' || args.key.length > 64 || typeof args.code !== 'string' || args.code.length > 64 ||
          !Number.isInteger(args.modifiers) || args.modifiers < 0 || args.modifiers > 15)
        return fail('dom_action_control_unavailable');
      const modifiers=args.modifiers;
      return ok({event_cancelled:!dispatch(new KeyboardEvent(step.name,{bubbles:true,composed:true,cancelable:true,key:args.key,code:args.code,
        altKey:!!(modifiers&1),ctrlKey:!!(modifiers&2),metaKey:!!(modifiers&4),shiftKey:!!(modifiers&8),repeat:false}))});
    }
    case 'contextmenu': case 'dblclick': case 'mouseover': case 'mouseenter':
      return ok({event_cancelled:!dispatch(new MouseEvent(step.name,{bubbles:step.name!=='mouseenter',composed:true,cancelable:true,
        button:step.name==='contextmenu'?2:0,detail:step.name==='dblclick'?2:0}))});
    case 'pointerover': case 'pointerenter':
      return ok({event_cancelled:!dispatch(new PointerEvent(step.name,{bubbles:step.name!=='pointerenter',composed:true,cancelable:true,pointerType:'mouse',isPrimary:true}))});
    case 'change': return ok({event_cancelled:!dispatch(new Event('change',{bubbles:true}))});
    default: return fail('dom_action_operation_unavailable');
  }
}"#;
