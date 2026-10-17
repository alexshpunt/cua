import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

// These execute shipped functions, not source wording. Node cannot qualify Blink/V8 annotations.
const source = await readFile(new URL("../dom_action_sample.rs", import.meta.url), "utf8");
const fixed = source.match(/const READ_FUNCTION: &str = r#"([\s\S]*?)"#;/)?.[1];
const hitSource = await readFile(new URL("../dom_action_hit.rs", import.meta.url), "utf8");
const hitFixed = hitSource.match(/const HIT_FUNCTION: &str = r#"([\s\S]*?)"#;/)?.[1];
assert.ok(fixed && hitFixed);
const setup = `
class Node {
  constructor(type=1) { this._type=type; this._children=[]; this._parentValue=null; this._connected=true; this._owner=null; }
  get _parent(){return this._parentValue;}
  set _parent(value){if(this._parentValue)this._parentValue._children=this._parentValue._children.filter(n=>n!==this);this._parentValue=value;if(value)value._children.push(this);}
  get nodeType(){return this._type;} get parentNode(){return this._parent;}
  get firstChild(){return this._children[0]||null;}
  get nextSibling(){return this._parent?this._parent._children[this._parent._children.indexOf(this)+1]||null:null;}
  get isConnected(){return this._connected;} get ownerDocument(){return this._owner;}
}
class NodeList { constructor(rows){this.rows=rows;} get length(){return this.rows.length;} item(i){return this.rows[i]||null;} }
const descendants=root=>root._children.flatMap(n=>[n,...descendants(n)]);
function query(selector){
 if(![':modal',':disabled'].includes(selector))throw Error('Unexpected selector');
 return new NodeList(descendants(this).filter(n=>selector===':modal'?n._modal===true:n._disabled===true));
}
class Element extends Node {
 constructor(){super();this._attrs={};this._rect={x:10.25,y:20.5,width:80,height:30};this._style={display:'block',visibility:'visible',interactivity:'auto'};this._tag='div';}
 get localName(){return this._tag;}
 getBoundingClientRect(){return this._rect;}
 getClientRects(){throw Error('embedder side effect denied');}
 matches(){throw Error('embedder side effect denied');}
 getAttribute(name){return Object.hasOwn(this._attrs,name)?this._attrs[name]:null;}
 hasAttribute(name){return Object.hasOwn(this._attrs,name);}
 get shadowRoot(){return this._shadow||null;} get assignedSlot(){return this._slot||null;}
}
class HTMLElement extends Element {get isContentEditable(){return this._editable===true;}}
class HTMLInputElement extends HTMLElement {get type(){return this._inputType||'text';}get readOnly(){return this._readonly===true;}}
class HTMLTextAreaElement extends HTMLElement {get readOnly(){return this._readonly===true;}}
class DocumentFragment extends Node {constructor(){super(11);}querySelectorAll(selector){return query.call(this,selector);}}
class ShadowRoot extends DocumentFragment {
 constructor(host){super();this._host=host;this._mode='open';}get host(){return this._host;}get mode(){return this._mode;}
 elementFromPoint(){throw Error('embedder side effect denied');}
}
class TreeWalker {nextNode(){throw Error('embedder side effect denied');}}
class Document extends Node {
 constructor(){super(9);}querySelectorAll(selector){return query.call(this,selector);}
 elementFromPoint(){throw Error('embedder side effect denied');}createTreeWalker(){throw Error('embedder side effect denied');}
}
const document=new Document(),getComputedStyle=node=>node._style;
class CSSStyleDeclaration {getPropertyValue(){throw Error('embedder side effect denied');}}
const CSS={supports(){throw Error('embedder side effect denied');}};
const window={innerWidth:800,innerHeight:600,scrollX:0,scrollY:0,visualViewport:{scale:1,offsetLeft:0,offsetTop:0}};
const make=(kind=HTMLElement,parent=document)=>{const node=new kind();node._owner=document;node._parent=parent;return node;};
const container=make(),node=make(HTMLInputElement,container);
`;
function sample(change="") {return JSON.parse(JSON.stringify(vm.runInNewContext(`${setup}\n${change}\n(${fixed})(node)`,{}, {timeout:1000})));}
function hit(change="") {return JSON.parse(JSON.stringify(vm.runInNewContext(`${setup}\nlet hit=node;\n${change}\n(${hitFixed})(node,hit)`,{}, {timeout:1000})));}

test("native accessors ignore page overrides while retaining fractional CSS boxes",()=>{
 const r=sample("node.getBoundingClientRect=()=>({x:999,y:999,width:999,height:999});node.matches=()=>true;");
 assert.deepEqual(r.bounds,[10.25,20.5,80,30]);assert.equal(r.enabled,true);assert.equal(r.editable,true);
 assert.equal(r.receives_pointer,null);assert.deepEqual(r.hit_test,{x:50,y:35,scroll_x:0,scroll_y:0,width:800,height:600});
});
test("readonly facts do not invoke embedder methods that side-effect checking rejects",()=>{
 const r=sample();assert.equal(r.visible,true);assert.equal(r.enabled,true);assert.equal(r.editable,true);assert.equal(r.receives_pointer,null);
});
test("inert self or ancestor cannot become an enabled editable control",()=>{
 for(const change of ["node._attrs.inert='';","container._attrs.inert='';","container._attrs.inert='false';"]){const r=sample(change);assert.equal(r.enabled,false);assert.equal(r.editable,false);}
});
test("effective inert ancestry follows assigned slots and open shadow hosts",()=>{
 const tree=`const host=make();const shadow=new ShadowRoot(host);shadow._owner=document;host._shadow=shadow;const inside=make(HTMLElement,shadow),slot=make(HTMLElement,inside);node._parent=host;node._slot=slot;`;
 for(const disabled of ["inside._attrs.inert='';","slot._attrs.inert='';","host._attrs.inert='';"]){const r=sample(tree+disabled);assert.equal(r.enabled,false);assert.equal(r.editable,false);}
 const r=sample(tree);assert.equal(r.enabled,true);assert.equal(r.editable,true);
});
test("modal document inertness and modal escape from ancestor inertness remain unavailable",()=>{
 for(const change of ["const dialog=make();dialog._modal=true;","container._attrs.inert='';const dialog=make(HTMLElement,container);dialog._modal=true;node._parent=dialog;","const host=make();const shadow=new ShadowRoot(host);shadow._owner=document;host._shadow=shadow;const dialog=make(HTMLElement,shadow);dialog._modal=true;"]){const r=sample(change);assert.equal(r.enabled,null);assert.equal(r.editable,null);}
});
test("modal discovery stops at its retained traversal budget instead of claiming absence",()=>{
 const r=sample("for(let i=0;i<2001;i++)make();");assert.equal(r.enabled,null);assert.equal(r.editable,null);
});
test("computed interactivity inert cannot become an enabled editable control",()=>{
 const direct=sample("node._style.interactivity='inert';");assert.equal(direct.enabled,false);assert.equal(direct.editable,false);
 const escaped=sample("container._style.interactivity='inert';");assert.equal(escaped.enabled,null);assert.equal(escaped.editable,null);
});
test("missing or contradictory CSS interactivity remains unknown without a support probe",()=>{
 for(const change of ["delete node._style.interactivity;","node._style.interactivity='';","node._style.interactivity='unexpected';"]){const r=sample(change);assert.equal(r.enabled,null);assert.equal(r.editable,null);}
});
test("native disabled, readonly, disconnected and unavailable ancestry remain distinct",()=>{
 assert.equal(sample("node._disabled=true;").editable,false);assert.equal(sample("node._readonly=true;").editable,false);assert.equal(sample("node._connected=false;").connected,false);
 const r=sample("for(let i=0;i<40;i++){const next=make(HTMLElement,container._parent);container._parent=next;}");assert.equal(r.enabled,null);assert.equal(r.editable,null);
});
test("hit point is integral document CSS with fixed rounding and truthful viewport limits",()=>{
 assert.deepEqual(sample("window.scrollY=120;").hit_test,{x:50,y:155,scroll_x:0,scroll_y:120,width:800,height:600});
 assert.equal(sample("node._rect={x:0.6,y:10,width:0.1,height:10};").hit_test,false);
 assert.equal(sample("node._rect.y=1000;").hit_test,false);
 assert.equal(sample("window.visualViewport.scale=2;").hit_test,null);
 assert.equal(sample("window.visualViewport.offsetTop=1;").hit_test,null);
});
test("native hit relation accepts exact or author/open-shadow descendants but not unrelated nodes",()=>{
 assert.equal(hit().relation,true);assert.equal(hit("hit=make(HTMLElement,node);").relation,true);
 assert.equal(hit("hit=make();").relation,false);
 assert.equal(hit("const shadow=new ShadowRoot(node);shadow._owner=document;node._shadow=shadow;hit=make(HTMLElement,shadow);").relation,true);
});
test("closed or UA shadow ancestry, foreign documents, frames and exhausted paths cannot grant a hit",()=>{
 for(const change of ["const shadow=new ShadowRoot(node);shadow._owner=document;shadow._mode='closed';hit=make(HTMLElement,shadow);","const shadow=new ShadowRoot(node);shadow._owner=document;shadow._mode='user-agent';hit=make(HTMLElement,shadow);","hit=make();hit._tag='iframe';","hit=make(HTMLElement,node);for(let i=0;i<70;i++)hit=make(HTMLElement,hit);"]){assert.equal(hit(change).relation,null);}
 for(const change of ["hit=make();hit._owner=new Document();","hit=make();hit._connected=false;"]){assert.equal(hit(change).connected,false);assert.equal(hit(change).relation,null);}
});
