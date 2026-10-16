import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

// Execute the actual fixed native operations; this is not a browser/live qualification.
const source = await readFile(new URL("../dom_action_operation.rs", import.meta.url), "utf8");
const preflight = source.match(/const PREFLIGHT_FUNCTION: &str = r#"([\s\S]*?)"#;/)?.[1];
const step = source.match(/const STEP_FUNCTION: &str = r#"([\s\S]*?)"#;/)?.[1];
assert.ok(preflight && step, "Both shipped fixed functions are executable.");
const setup = `
class Node {
  constructor() { this._connected = true; this._owner = document; this._parent = null; }
  get isConnected() { return this._connected; }
  get ownerDocument() { return this._owner; }
  get parentNode() { return this._parent; }
  getRootNode() { return this._root || this._owner; }
}
class EventTarget {
  dispatchEvent(event) {
    this._events.push({type:event.type, trusted:false, key:event.key, code:event.code});
    if (this._throw === event.type) throw Error('page exception');
    return !this._cancel?.includes(event.type);
  }
}
Object.setPrototypeOf(Node.prototype, EventTarget.prototype);
class Element extends Node {
  constructor() { super(); this._events = []; }
  scrollIntoView(options) { this._scroll = options; }
}
class HTMLElement extends Element {
  click() { this._clicks = (this._clicks || 0) + 1; if (this instanceof HTMLInputElement && this._type === 'checkbox') this._checked = !this._checked; }
  focus(options) { this._focusOptions = options; (this._root || document)._active = this; }
}
class HTMLInputElement extends HTMLElement {
  constructor() { super(); this._type = 'text'; this._value = ''; this._start = 0; this._end = 0; this._checked = false; }
  get type() { return this._type; }
  get value() { return this._value; }
  set value(value) { this._value = value; this._start = this._end = value.length; }
  get selectionStart() { return this._start; }
  get selectionEnd() { return this._end; }
  get checked() { return this._checked; }
  setRangeText(text, start, end, mode) { assertSelection(mode); this._value = this._value.slice(0,start) + text + this._value.slice(end); this._start = this._end = start + text.length; }
}
class HTMLTextAreaElement extends HTMLElement {
  constructor() { super(); this._value = ''; this._start = 0; this._end = 0; }
  get value() { return this._value; }
  set value(value) { this._value = value; this._start = this._end = value.length; }
  get selectionStart() { return this._start; }
  get selectionEnd() { return this._end; }
  setRangeText(text, start, end, mode) { assertSelection(mode); this._value = this._value.slice(0,start) + text + this._value.slice(end); this._start = this._end = start + text.length; }
}
const assertSelection = mode => { if(mode !== 'end') throw Error('Unexpected selection mode'); };
class HTMLOptionElement extends HTMLElement {
  constructor(value) { super(); this._value = value; this._selected = false; this._disabled = false; }
  get value() { return this._value; }
  get selected() { return this._selected; }
  set selected(value) { this._selected = value; }
  get disabled() { return this._disabled; }
}
class HTMLOptGroupElement extends HTMLElement { get disabled() { return this._disabled === true; } }
class HTMLCollection { constructor(values) { this._values = values; } get length() { return this._values.length; } item(index) { return this._values[index] || null; } }
class HTMLOptionsCollection extends HTMLCollection {}
class HTMLSelectElement extends HTMLElement {
  constructor() { super(); this._options = new HTMLOptionsCollection([]); this._multiple = false; }
  get options() { return this._options; }
  get multiple() { return this._multiple; }
}
class Document { get activeElement() { return this._active || null; } }
class ShadowRoot { get activeElement() { return this._active || null; } }
class Event { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
class MouseEvent extends Event {}
class PointerEvent extends MouseEvent {}
class KeyboardEvent extends Event {}
class InputEvent extends Event {}
const document = new Document();
const node = new HTMLInputElement();
`;
function execute({ change = "", request = { action: "click" }, steps = [] } = {}) {
  const context = vm.createContext({});
  vm.runInContext(setup + change, context, { timeout: 1000 });
  const result = vm.runInContext(`(${preflight})(node, ${JSON.stringify(request)})`, context, { timeout: 1000 });
  const results = [];
  for (const next of steps) results.push(vm.runInContext(`(${step})(node, ${JSON.stringify(next)})`, context, { timeout: 1000 }));
  return JSON.parse(JSON.stringify({ result, results, state:vm.runInContext(`({clicks:node._clicks || 0,value:node._value,checked:node._checked,events:node._events,focusOptions:node._focusOptions,scroll:node._scroll,selected:node._options?node._options._values.map(option => option._selected):null})`,context) }));
}
const operation = (name, argument = {}) => ({ name, argument });

test("native click and exact-node checkbox toggles ignore page overrides", () => {
  const result = execute({ change:"node._type = 'checkbox'; node.click = () => { throw Error('page override'); };", request:{action:"check"}, steps:[operation("set_checked",{checked:true}),operation("set_checked",{checked:true})] });
  assert.equal(result.result.ok,true); assert.equal(result.state.checked,true); assert.equal(result.state.clicks,1);
  assert.equal(result.results[1].observed.already_matched,true);
});
test("unsupported controls and non-checkbox uncheck refuse before mutation", () => {
  for (const [action, change] of [["uncheck","node._type='radio';"],["type","node._type='number';"],["fill","node._type='email';"]]) {
    const result = execute({change,request:{action,text:"secret"}});
    assert.equal(result.result.ok,false); assert.equal(result.result.reason,"dom_action_control_unavailable"); assert.equal(result.state.clicks,0); assert.deepEqual(result.state.events,[]);
  }
});
test("fill and insertion use native setters and preserve selection semantics", () => {
  const result = execute({change:"node._value='abc'; node._start=1; node._end=2; node.setRangeText=()=>{throw Error('page override');};",request:{action:"type",text:"🙂"}, steps:[operation("focus"),operation("beforeinput",{text:"🙂",input_type:"insertText"}),operation("insert_text",{text:"🙂"}),operation("input",{text:"🙂",input_type:"insertText"})]});
  assert.equal(result.result.ok,true); assert.equal(result.state.value,"a🙂c"); assert.deepEqual(result.state.events.map(event=>event.type),["beforeinput","input"]); assert.deepEqual(result.state.focusOptions,{preventScroll:true});
  const fill = execute({change:"Object.defineProperty(node,'value',{set(){throw Error('page override');}});",request:{action:"fill",text:""},steps:[operation("fill",{text:""})]});
  assert.equal(fill.results[0].observed.value_matches,true);
});
test("cancelled beforeinput is explicit and does not apply text", () => {
  const result = execute({change:"node._value='old'; node._cancel=['beforeinput'];",request:{action:"fill",text:"new"},steps:[operation("beforeinput",{text:"new",input_type:"insertReplacementText"})]});
  assert.equal(result.results[0].reason,"dom_action_event_cancelled"); assert.equal(result.state.value,"old");
});
test("selection preflight refuses ambiguity, disabled options and excessive collections", () => {
  const select = "const select = new HTMLSelectElement(); Object.setPrototypeOf(node,HTMLSelectElement.prototype); node._options=select._options; node._multiple=false;";
  for(const suffix of ["node._options._values=[new HTMLOptionElement('a'),new HTMLOptionElement('a')];","const option=new HTMLOptionElement('a'); option._disabled=true; node._options._values=[option];","node._options._values=Array.from({length:201},()=>new HTMLOptionElement('a')); "]) {
    const result=execute({change:select+suffix,request:{action:"select_option",values:["a"]}}); assert.equal(result.result.ok,false); assert.deepEqual(result.state.events,[]);
  }
});
test("native option selection dispatches only explicit input/change steps", () => {
  const change="Object.setPrototypeOf(node,HTMLSelectElement.prototype); node._options=new HTMLOptionsCollection([new HTMLOptionElement(''),new HTMLOptionElement('second')]); node._multiple=false;";
  const result=execute({change,request:{action:"select_option",values:[""]},steps:[operation("select_values",{values:[""]}),operation("input",{input_type:null,text:null}),operation("change")]});
  assert.equal(result.result.ok,true); assert.deepEqual(result.state.selected,[true,false]); assert.equal(result.results[0].observed.selected_matches,true); assert.deepEqual(result.state.events.map(event=>event.type),["input","change"]);
});
test("synthetic keyboard events do not insert text or claim trusted defaults", () => {
  const key={key:"a",code:"KeyA",modifiers:2};
  const result=execute({request:{action:"press",key:"Control+a"},steps:[operation("focus"),operation("keydown",key),operation("keyup",key)]});
  assert.equal(result.state.value,""); assert.deepEqual(result.state.events.map(event=>event.type),["keydown","keyup"]); assert.ok(result.state.events.every(event=>event.trusted===false));
});
test("pointer-like DOM events and scrolling use no coordinate substitution", () => {
  const result=execute({steps:[operation("contextmenu"),operation("pointerover"),operation("mouseover"),operation("pointerenter"),operation("mouseenter"),operation("dblclick"),operation("scroll_into_view")]});
  assert.deepEqual(result.state.events.map(event=>event.type),["contextmenu","pointerover","mouseover","pointerenter","mouseenter","dblclick"]); assert.deepEqual(result.state.scroll,{block:"nearest",inline:"nearest",behavior:"instant"});
});
test("disconnected/foreign nodes and unknown operations refuse without dispatch", () => {
  for(const change of ["node._connected=false;","node._owner=new Document();"]) { const result=execute({change,steps:[operation("click")]}); assert.equal(result.result.reason,"dom_action_ref_stale"); assert.equal(result.results[0].reason,"dom_action_ref_stale"); assert.equal(result.state.clicks,0); }
  const unknown=execute({steps:[operation("eval",{text:"anything"})]}); assert.equal(unknown.results[0].reason,"dom_action_operation_unavailable"); assert.deepEqual(unknown.state.events,[]);
});
test("page event exceptions remain failures rather than acknowledged dispatch", () => {
  assert.throws(()=>execute({change:"node._throw='input';",steps:[operation("input",{text:"text",input_type:"insertText"})]}),/page exception/);
});
