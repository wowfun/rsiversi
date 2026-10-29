import assert from 'node:assert/strict';
import {test} from 'node:test';
import {externalPaneClass as controllerClass} from '../src/runtime/external-controller.js';
class Element {
  children=[]; dataset={}; value=''; scrollHeight=0; scrollTop=0; clientHeight=0;
  listeners={}; setAttribute() {} addEventListener(name,handler) {this.listeners[name]=handler;} append(...nodes) {this.children.push(...nodes);}
  replaceChildren(...nodes) {this.clears=(this.clears??0)+1;this.children=nodes;}
}

function externalPaneClass(ports){const Controller=controllerClass(ports);return class extends Controller {constructor(index){super(index);this.mount({node:new Element(),input:new Element()})}}}
const frame=(id,generation=id)=>({generation,external:{observed:{connected:true,snapshot:{id,generation:'1',endpoint:'fixture',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:[],following:true,busy:false}});
const edit=(pane,text)=>{pane.input.value=text;pane.input.listeners.input();};

function setup(t,command) {
  const original=globalThis.document;
  globalThis.document={getElementById:()=>new Element()};
  t.after(()=>{globalThis.document=original;});
  return externalPaneClass({element:()=>new Element(),button:()=>new Element(),command,perform:work=>work()});
}

test('capacity notices follow accepted revisions across panes and survive blocked sends',async t=>{
  const Pane=setup(t,async()=>{throw Error('unknown');}), panes=[];
  for(let i=0;i<16;i++){const pane=new Pane(i);pane.render(frame(String(i)));edit(pane,'x'.repeat(65536));panes.push(pane);}
  const first=panes[0], sibling=new Pane(20);sibling.render(frame('0'));
  await assert.rejects(first.submit());edit(first,first.input.value+'x');
  await first.submit();assert.match(first.composerView.notice,/capacity/);
  edit(sibling,'short');assert.doesNotMatch(first.composerView.notice,/capacity/);
  edit(first,'x'.repeat(65537));assert.match(first.composerView.notice,/capacity/);
  edit(sibling,'');assert.equal(first.input.value,'');assert.doesNotMatch(first.composerView.notice,/capacity/);
  edit(first,'x'.repeat(65537));await first.submit();assert.match(first.composerView.notice,/capacity/);
});

test('unknown external revision survives navigation, reattachment and no-op input',async t=>{
  let calls=0;
  const Pane=setup(t,async()=>{calls++;throw Error('unknown');}), pane=new Pane(0);
  pane.render(frame('A'));edit(pane,'original');
  await assert.rejects(pane.submit());
  assert.equal(pane.composerView.sendDisabled,true);
  pane.render(frame('B'));edit(pane,'other');pane.render(frame('A','A2'));
  assert.equal(pane.input.value,'original');assert.equal(pane.uncertain,true);
  edit(pane,'original');await pane.submit();assert.equal(calls,1);
  pane.render(frame('A','A3'));await pane.submit();assert.equal(calls,1);
  edit(pane,'edited');assert.equal(pane.uncertain,false);
  await assert.rejects(pane.submit());assert.equal(calls,2);
});

test('a capacity-blocked pane keeps history stable and recovers when a binding is freed',t=>{
  const Pane=setup(t,async()=>{}), pinned=[];
  for(let i=0;i<16;i++){const pane=new Pane(i);pane.render(frame(String(i)));pinned.push(pane);}
  const pane=new Pane(16), state=frame('overflow');
  state.external.blocks=[{key:'one',role:'agent',text:'Observed history',source:{}}];
  pane.render(state);
  const row=pane.data.blocks[0];
  pane.render(state);pane.render(state);
  assert.equal(pane.data.blocks[0],row);
  assert.equal(pane.composerView.inputDisabled,true);
  assert.equal(pane.composerView.sendDisabled,true);
  pinned[0].reset();pane.render(state);
  assert.equal(pane.composerView.inputDisabled,false);
  assert.equal(pane.data.blocks[0],row);
  edit(pane,'Recovered draft');
  assert.equal(pane.input.value,'Recovered draft');assert.equal(pane.composerView.sendDisabled,false);
});

test('edits render once and a capacity rejection retains the unknown-outcome warning',async t=>{
  const Pane=setup(t,async()=>{throw Error('unknown');}), panes=[];
  for(let i=0;i<16;i++){const pane=new Pane(i);pane.render(frame(String(i)));edit(pane,'x'.repeat(65536));panes.push(pane);}
  const pane=panes[0];await assert.rejects(pane.submit());
  let renders=0;const render=pane.renderComposer.bind(pane);pane.renderComposer=()=>{renders++;render();};
  edit(pane,pane.input.value+'x');
  assert.equal(pane.input.value.length,65536);
  assert.match(pane.composerView.notice,/outcome is unknown/);
  assert.match(pane.composerView.notice,/capacity/);
  assert.equal(renders,1);
  renders=0;edit(pane,'changed');assert.equal(renders,1);assert.equal(pane.uncertain,false);
});

test('rejected edits leave retained text sendable by button and Enter',async t=>{
  const calls=[];
  const Pane=setup(t,async value=>calls.push(value)), panes=[];
  for(let i=0;i<16;i++){const pane=new Pane(i);pane.render(frame(String(i)));edit(pane,'x'.repeat(65536));panes.push(pane);}
  const pane=panes[0];
  edit(pane,pane.input.value+'x');
  assert.equal(pane.input.value.length,65536);
  assert.match(pane.composerView.notice,/capacity/);
  assert.equal(pane.composerView.sendDisabled,false,'the restored admitted text can still be sent');
  await pane.submit();
  assert.equal(calls.length,1);assert.equal(calls[0].command.text.length,65536);
  edit(pane,'x'.repeat(65536));edit(pane,pane.input.value+'x');
  pane.input.listeners.keydown({key:'Enter',preventDefault(){}});
  await Promise.resolve();await Promise.resolve();
  assert.equal(calls.length,2);assert.equal(calls[1].command.text.length,65536);
});

test('reset clears failed-admission presentation state',t=>{
  const Pane=setup(t,async()=>{}), pane=new Pane(0);
  pane.render(frame('A'));edit(pane,'x'.repeat(1024*1024+1));
  assert.equal(pane.capacity,true);
  pane.reset();
  assert.equal(pane.capacity,false);assert.equal(pane.bindingVersion,undefined);
  assert.equal(pane.composerView.notice,'');assert.equal(pane.composerView.inputDisabled,true);
});

test('late completions only mutate their original revision, including ABA edits',async t=>{
  let finish,fail,calls=0;
  const Pane=setup(t,()=>{calls++;return new Promise((resolve,reject)=>{finish=resolve;fail=reject;});});
  const pane=new Pane(0);
  pane.render(frame('A'));edit(pane,'same');const first=pane.submit();
  pane.render(frame('B'));edit(pane,'same');finish();await first;
  assert.equal(pane.input.value,'same');pane.render(frame('A','A2'));assert.equal(pane.input.value,'');
  edit(pane,'same');const second=pane.submit();edit(pane,'changed');edit(pane,'same');finish();await second;
  assert.equal(pane.input.value,'same');
  const third=pane.submit();pane.render(frame('B','B2'));fail(Error('unknown'));await assert.rejects(third);
  assert.equal(pane.uncertain,false);assert.equal(pane.input.value,'same');
  pane.render(frame('A','A3'));assert.equal(pane.uncertain,true);await pane.submit();assert.equal(calls,3);
});

test('two panes share drafts and a pending attempt, connection reset fences old completion',async t=>{
  let finish,calls=0;
  const Pane=setup(t,()=>{calls++;return new Promise(resolve=>{finish=resolve;});});
  const a=new Pane(0),b=new Pane(1);a.render(frame('A'));b.render(frame('A'));
  edit(a,'shared');assert.equal(b.input.value,'shared');const sending=a.submit();
  assert.equal(b.submitting,true);await b.submit();assert.equal(calls,1);
  edit(b,'new');assert.equal(a.input.value,'new');finish();await sending;assert.equal(a.input.value,'new');
  const old=a.submit();Pane.clearDrafts();assert.equal(a.input.value,'');assert.equal(b.input.value,'');
  a.render(frame('A','new-connection'));edit(a,'new');finish();await old;
  assert.equal(a.input.value,'new');assert.equal(a.uncertain,false);
});

test('external switch survives old frames and prevents keyboard submission until new binding',async()=>{
  const original=globalThis.document, calls=[];
  globalThis.document={getElementById:()=>new Element()};
  try {
    const Pane=externalPaneClass({element:()=>new Element(),button:()=>new Element(),command:async value=>calls.push(value),perform:work=>work()});
    const pane=new Pane(0);
    const frame=generation=>({generation,external:{observed:{connected:true,snapshot:{id:'external',generation:'1',endpoint:'fixture',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:[],following:true,busy:false}});
    pane.render(frame('one')); pane.input.value='one prompt'; pane.switching=true;
    pane.render(frame('one'));
    assert.equal(pane.switching,true); assert.equal(pane.composerView.inputDisabled,true); assert.equal(pane.composerView.sendDisabled,true);
    await pane.submit(); assert.equal(calls.length,0);
    pane.render(frame('two'));
    assert.equal(pane.switching,false); assert.equal(pane.composerView.inputDisabled,false);
    await pane.submit(); assert.equal(calls.length,1); assert.equal(calls[0].generation,'two');
  }finally{globalThis.document=original;}
});

test('external composer shares Enter preference, modifier and IME handling',async()=>{
  const original=globalThis.document;
  globalThis.document={getElementById:()=>new Element()};
  try {
    const Pane=externalPaneClass({element:()=>new Element(),button:()=>new Element(),command:async()=>{},perform:work=>work()});
    const pane=new Pane(0);let sends=0,prevented=0;
    pane.submit=async()=>{sends++};
    for(const [submitKey,extra,expected] of [
      ['enter',{},true],['mod_enter',{},false],['mod_enter',{ctrlKey:true},true],
      ['enter',{metaKey:true},true],['enter',{altKey:true},false],['enter',{shiftKey:true},false],
      ['enter',{isComposing:true},false],['enter',{keyCode:229},false],
    ]){
      pane.submitKey=submitKey;const before=sends;
      pane.input.listeners.keydown({key:'Enter',preventDefault:()=>prevented++,...extra});
      assert.equal(sends-before,Number(expected));
    }
    assert.equal(prevented,sends);
  }finally{globalThis.document=original;}
});
