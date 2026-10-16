import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

// Execute the shipped fixed function, not a second implementation of its ancestry rules.
const source = await readFile(new URL("../dom_action_sample.rs", import.meta.url), "utf8");
const fixed = source.match(/const READ_FUNCTION: &str = r#"([\s\S]*?)"#;/)?.[1];
assert.ok(fixed, "The compiled Rust sample must contain its executable function.");
const setup = `
class Node {
  constructor(type = 1) { this._type = type; this._children = []; this._parentValue = null; this._connected = true; this._owner = null; }
  get _parent() { return this._parentValue; }
  set _parent(value) {
    if (this._parentValue) this._parentValue._children = this._parentValue._children.filter(node => node !== this);
    this._parentValue = value; if (value) value._children.push(this);
  }
  get nodeType() { return this._type; }
  get parentNode() { return this._parent; }
  get isConnected() { return this._connected; }
  get ownerDocument() { return this._owner; }
  contains(other) { for (let count = 0; other && count < 100; count++, other = other._parent) if (other === this) return true; return false; }
}
class Element extends Node {
  constructor() { super(); this._attrs = {}; this._rect = {x:10.25,y:20.5,width:80,height:30}; this._style = {display:'block',visibility:'visible'}; }
  getBoundingClientRect() { return this._rect; }
  getClientRects() { return this._boxes === false ? [] : [this._rect]; }
  matches(selector) {
    if (selector === ':modal') return this._modal === true;
    if (selector !== ':disabled') throw Error('Unexpected selector'); return this._disabled === true;
  }
  getAttribute(name) { return Object.hasOwn(this._attrs, name) ? this._attrs[name] : null; }
  hasAttribute(name) { return Object.hasOwn(this._attrs, name); }
  get shadowRoot() { return this._shadow || null; }
  get assignedSlot() { return this._slot || null; }
}
class HTMLElement extends Element { get isContentEditable() { return this._editable === true; } }
class HTMLInputElement extends HTMLElement {
  get type() { return this._inputType || 'text'; }
  get readOnly() { return this._readonly === true; }
}
class HTMLTextAreaElement extends HTMLElement { get readOnly() { return this._readonly === true; } }
class ShadowRoot extends Node {
  constructor(host) { super(11); this._host = host; }
  get host() { return this._host; }
  elementFromPoint() { return this._hit || null; }
}
const descendants = root => root._children.flatMap(node => [node, ...descendants(node)]);
class TreeWalker {
  constructor(root) { this.nodes = descendants(root); }
  nextNode() { return this.nodes.shift() || null; }
}
const NodeFilter = {SHOW_ALL:4294967295};
class Document extends Node {
  constructor() { super(9); }
  elementFromPoint() { return this._hit || null; }
  createTreeWalker(root, filter) { if (filter !== 4294967295) throw Error('Unexpected traversal'); return new TreeWalker(root); }
}
const document = new Document();
const getComputedStyle = node => node._style;
class CSSStyleDeclaration {
  getPropertyValue(name) { if (name !== 'interactivity') throw Error('Unexpected property'); return this.interactivity ?? (CSS._supported ? 'auto' : ''); }
}
const CSS = { _supported:false, supports(property, value) {
  if (property !== 'interactivity' || value !== 'inert') throw Error('Unexpected support probe'); return this._supported;
} };
const make = (kind = HTMLElement, parent = document) => { const node = new kind(); node._owner = document; node._parent = parent; return node; };
const container = make();
const node = make(HTMLInputElement, container);
document._hit = node;
`;
function sample(change = "") {
  return JSON.parse(JSON.stringify(vm.runInNewContext(`${setup}\n${change}\n(${fixed})(node)`, {}, { timeout: 1000 })));
}

test("native accessors ignore page overrides while retaining fractional CSS boxes", () => {
  const result = sample("node.getBoundingClientRect = () => ({x:999,y:999,width:999,height:999}); node.matches = () => true;");
  assert.deepEqual(result.bounds, [10.25, 20.5, 80, 30]);
  assert.equal(result.enabled, true); assert.equal(result.editable, true); assert.equal(result.receives_pointer, true);
});
test("inert self or ancestor cannot become an enabled editable control", () => {
  for (const change of ["node._attrs.inert = '';", "container._attrs.inert = '';", "container._attrs.inert = 'false';"]) {
    const result = sample(change);
    assert.equal(result.enabled, false); assert.equal(result.editable, false);
  }
});
test("effective inert ancestry follows assigned slots and open shadow hosts", () => {
  const tree = `const host = make(); const shadow = new ShadowRoot(host); shadow._owner = document;
    host._shadow = shadow; const inside = make(HTMLElement, shadow); const slot = make(HTMLElement, inside);
    node._parent = host; node._slot = slot; document._hit = host; shadow._hit = node;`;
  for (const disabled of ["inside._attrs.inert = '';", "slot._attrs.inert = '';", "host._attrs.inert = ''; "]) {
    const result = sample(tree + disabled);
    assert.equal(result.enabled, false); assert.equal(result.editable, false);
  }
  const active = sample(tree);
  assert.equal(active.enabled, true); assert.equal(active.editable, true); assert.equal(active.receives_pointer, true);
});
test("modal document inertness and modal escape from ancestor inertness remain unavailable", () => {
  for (const change of [
    "const dialog = make(); dialog._modal = true;",
    "container._attrs.inert = ''; const dialog = make(HTMLElement, container); dialog._modal = true; node._parent = dialog;",
    "const host = make(); const shadow = new ShadowRoot(host); shadow._owner = document; host._shadow = shadow; const dialog = make(HTMLElement, shadow); dialog._modal = true;",
  ]) {
    const result = sample(change);
    assert.equal(result.enabled, null); assert.equal(result.editable, null);
  }
});
test("modal discovery stops at its retained traversal budget instead of claiming absence", () => {
  const result = sample("for (let i = 0; i < 2001; i++) make();");
  assert.equal(result.enabled, null); assert.equal(result.editable, null);
});
test("computed interactivity inert cannot become an enabled editable control", () => {
  const direct = sample("CSS._supported = true; node._style.interactivity = 'inert';");
  assert.equal(direct.enabled, false); assert.equal(direct.editable, false);
  const ambiguousEscape = sample("CSS._supported = true; container._style.interactivity = 'inert';");
  assert.equal(ambiguousEscape.enabled, null); assert.equal(ambiguousEscape.editable, null);
});
test("unsupported CSS interactivity differs from missing or contradictory native computed evidence", () => {
  assert.equal(sample().enabled, true);
  for (const change of ["CSS._supported = true; node._style.interactivity = '';", "CSS._supported = true; node._style.interactivity = 'unexpected';"])
    assert.equal(sample(change).enabled, null);
});
test("native disabled, readonly, disconnected and unavailable ancestry remain distinct", () => {
  assert.equal(sample("node._disabled = true;").editable, false);
  assert.equal(sample("node._readonly = true;").editable, false);
  assert.equal(sample("node._connected = false;").connected, false);
  const unknown = sample("for (let i=0;i<40;i++) { const next = make(HTMLElement, container._parent); container._parent = next; }");
  assert.equal(unknown.enabled, null); assert.equal(unknown.editable, null);
});
