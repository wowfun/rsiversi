import {test} from 'node:test';
import assert from 'node:assert/strict';
import {prepareComposer} from '../src/runtime/starting-conversation.js';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {StartingDraft} from '../starting-draft.js';

test('a blank surface binds its draft after reset and same-namespace reconnect',async t=>{
  let loads=0;
  t.mock.method(StartingDraft,'load',async()=>{loads++;return {text:'retained',workspace:'workspace'};});
  const context={connection:{drafts:{namespace:['endpoint','principal']},closing:true},view:{surfaces:{main:null},catalog:{workspaces:[{id:'workspace'}]}},notify:assert.fail};
  const pane=new (createPaneClass(context))('main');
  pane.reset();
  await pane.startBinding;
  context.connection={drafts:{namespace:['endpoint','principal']},closing:false};
  pane.render(null);
  await pane.startBinding;
  assert.equal(pane.starting?.text,'retained');
  assert.equal(loads,1);
});

test('a retired starting draft load cannot replace a newer binding',async t=>{
  let release, loads=0;
  t.mock.method(StartingDraft,'load',async()=>++loads===1?new Promise(resolve=>{release=resolve}):{text:'current'});
  const context={connection:{drafts:{namespace:['endpoint','principal']}},view:{surfaces:{main:null},catalog:{workspaces:[]}},notify:assert.fail};
  const pane=new (createPaneClass(context))('main');
  pane.render(null);const retired=pane.startBinding;
  pane.reset();await pane.startBinding;
  release({text:'retired'});await retired;
  assert.equal(pane.starting.text,'current');
});
test('opening concurrent composer controls creates once, preserves text, and never submits',async()=>{
  let release,creates=0,cleared=false;
  const gate=new Promise(resolve=>{release=resolve;});
  const editor={text:'',images:[],references:[],record:{},edit(text){this.text=text;},async flush(){}};
  const pane={index:'main',starting:{text:'saved input',serial:3,async flush(){await gate;},async clearCaptured(serial){assert.equal(serial,3);cleared=true;}},renderComposer(){},subscribe(){return()=>{};},submit(){throw Error('must not submit');}};
  const connection={};const context={connection,view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>async()=>{creates++;pane.generation='1';pane.editor=editor;pane.staged={data:{workspace:'workspace'}};}};
  const one=prepareComposer(pane,context),two=prepareComposer(pane,context);release();
  assert.deepEqual(await Promise.all([one,two]),[true,true]);assert.equal(creates,1);assert.equal(editor.text,'saved input');assert(cleared);
});
test('failed draft creation keeps starting input and releases control admission',async()=>{
  let cleared=false;
  const pane={index:'main',starting:{text:'retained',async flush(){},async clearCaptured(){cleared=true;}},renderComposer(){}};
  const context={connection:{},view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>async()=>{throw Error('creation failed');}};
  await assert.rejects(prepareComposer(pane,context),/creation failed/);assert.equal(cleared,false);assert.equal(pane.submitting,false);assert.equal(pane.openingComposer,undefined);
});
