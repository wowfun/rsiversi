import {test} from 'node:test';
import assert from 'node:assert/strict';
import {prepareComposer,restoreStartingDraft} from '../src/runtime/starting-conversation.js';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {StartingDraft} from '../starting-draft.js';

test('starting recovery stops after connection replacement or closing its recovery panel',async t=>{
  for(const replacement of [false,true]) {
    const gate=Promise.withResolvers(),entered=Promise.withResolvers();
    const row={key:JSON.stringify(['endpoint','local','main']),incarnation:'original',revision:1};
    const load=t.mock.method(StartingDraft,'load',async()=>{entered.resolve();await gate.promise;return {record:row,text:'original input',serial:0,async flush(){},clearCaptured:assert.fail};});
    const panel={},pane={index:'main',recoveryView:panel,async flush(){},renderComposer(){}};
    const connection={drafts:{namespace:['endpoint','local']}};
    const context={connection,view:{setup:{agent:{default_model:{}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>assert.fail('retired recovery must not create')};
    const restoring=restoreStartingDraft(pane,context,row);
    await entered.promise;
    if(replacement)context.connection={drafts:{namespace:['other','local']}};
    else pane.recoveryView=undefined;
    gate.resolve();await assert.rejects(restoring,/changed|closed/i);
    load.mock.restore();
  }
});

test('opening controls on a blank starting page does not persist an empty editor',async()=>{
  let cleared=false;
  const editor={text:'',images:[],references:[],record:{},edit:assert.fail,flush:assert.fail};
  const pane={index:'main',starting:{text:'',workspace:'workspace',serial:0,async flush(){},async clearCaptured(){cleared=true;}},renderComposer(){},subscribe(){return()=>{};}};
  const context={connection:{},view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},
    boundCall:()=>async()=>{pane.editor=editor;pane.generation='new';pane.staged={data:{creation:{workspace_id:'workspace'}}};}};
  assert.equal(await prepareComposer(pane,context),true);
  assert.equal(cleared,true);
});

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
  const connection={drafts:{namespace:['endpoint','local']}};const context={connection,view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>async()=>{creates++;pane.generation='1';pane.editor=editor;pane.staged={data:{creation:{workspace_id:'workspace'},workspace_key:'coordinate-key'}};}};
  const one=prepareComposer(pane,context),two=prepareComposer(pane,context);release();
  assert.deepEqual(await Promise.all([one,two]),[true,true]);assert.equal(creates,1);assert.equal(editor.text,'saved input');assert(cleared);
});
test('failed draft creation keeps starting input and releases control admission',async()=>{
  let cleared=false;
  const pane={index:'main',starting:{text:'retained',async flush(){},async clearCaptured(){cleared=true;}},renderComposer(){}};
  const context={connection:{},view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>async()=>{throw Error('creation failed');}};
  await assert.rejects(prepareComposer(pane,context),/creation failed/);assert.equal(cleared,false);assert.equal(pane.submitting,undefined);assert.equal(pane.openingComposer,undefined);
});

test('recovery waits for the newly created attachment and never sends restored input',async t=>{
  let check,cleared=false;
  const row={key:JSON.stringify(['endpoint','local','old-pane']),incarnation:'incarnation',revision:3};
  t.mock.method(StartingDraft,'load',async()=>({record:row,text:'recovered input',serial:0,workspace:'workspace',async flush(){},async clearCaptured(){cleared=true;}}));
  const old={text:'',images:[],references:[],record:{},edit(){assert.fail('Old attachment was edited');}};
  const fresh={text:'',images:[],references:[],record:{},edit(text){this.text=text;},async flush(){}};
  const pane={index:'main',generation:'1',editor:old,staged:{data:{creation:{workspace_id:'workspace'}}},async flush(){},renderComposer(){},subscribe(fn){check=fn;return()=>{};},submit:assert.fail};
  const connection={drafts:{namespace:['endpoint','local']}};const context={connection,view:{setup:{agent:{default_model:{model:'test'}}},catalog:{workspaces:[{id:'workspace'}]}},boundCall:()=>async()=>{}};
  const restoring=restoreStartingDraft(pane,context,row);
  while(!check)await new Promise(resolve=>setImmediate(resolve));
  assert.equal(cleared,false);pane.generation='2';pane.editor=fresh;check();
  assert.equal(await restoring,true);assert.equal(fresh.text,'recovered input');assert(cleared);
});
