import {createInteraction, interactionKey} from '../src/runtime/interaction.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import {approvalFields,approvalArguments,approvalReason,approvalJson,decisionAction} from '../approval-review.js';
test('approval describes supplied execution evidence without claiming enforcement',()=>{
  const review={cwd:'/project',sandbox:'workspace-write',arguments:{}};
  assert.deepEqual(approvalFields(null),[]);
  assert.deepEqual(approvalFields(review),[['Execution location','Not supplied'],['Working directory','/project'],['Requested sandbox policy','workspace-write']]);
  for(const [location,label] of [[{kind:'local'},'Service machine'],[{kind:'ssh',target:'server'},'SSH · server'],[{kind:'future'},'Unrecognized execution location']]) {
    assert.deepEqual(approvalFields({...review,execution:{binding:{location},workspace:'/workspace'}}),[['Execution location',label],['Working directory','/project'],['Workspace','/workspace'],['Requested sandbox policy','workspace-write']]);
  }
});
test('approval retains all ordered arguments including long, nested and non-object values',()=>{
  const value={command:'<script>'+'x'.repeat(65536),nested:{array:[true,null,'你好']}};
  const rows=approvalArguments(value);
  assert.deepEqual(rows.map(([key])=>key),['command','nested']);
  assert.deepEqual(Object.fromEntries(rows.map(([key,text])=>[key,JSON.parse(text)])),value);
  for(const value of [null,[],42,'text']) assert.deepEqual(approvalArguments(value),[['Value',JSON.stringify(value,null,2)]]);
});

test('machine reasons have plain presentation while custom reasons remain exact',()=>{
  assert.equal(approvalReason('Agent turn message-abc123 requested this Tool effect'),'The assistant requested this tool action.');
  assert.equal(approvalReason('Read the selected workspace file.'),'Read the selected workspace file.');
  assert.equal(approvalReason('Agent turn something else'),'Agent turn something else');
});

test('large approval arguments and raw JSON have independent bounded UTF-8 windows',()=>{
  const value={large:'界'.repeat(1024*1024),tail:'must be visibly omitted'};
  const fields=approvalArguments(value);
  const text=fields.flat().join('');
  assert.ok(new TextEncoder().encode(text).length<=256*1024);
  assert.match(text,/truncated/);
  assert.ok(!text.includes('\uFFFD'));
  const raw=approvalJson({review:{arguments:value}});
  assert.ok(new TextEncoder().encode(raw).length<=256*1024);
  assert.match(raw,/truncated/);
});

test('an exactly fitting final argument is not truncated to reserve a later-field marker',()=>{
  const value={last:'x'.repeat(256*1024-6)};
  assert.deepEqual(approvalArguments(value),[['last',JSON.stringify(value.last)]]);
});


test('human decisions refuse duplicate clicks and recover only known admission refusals',async()=>{
  let calls=0, reject, disabled; const pending=[],reports=[];
  const decide=decisionAction(()=>{calls++;return new Promise((_,fail)=>{reject=fail})},value=>{disabled=value},text=>reports.push(text),value=>pending.push(value));
  const first=decide(true); await decide(false); assert.equal(calls,1); assert.equal(disabled,true);
  reject(Object.assign(new Error('busy'),{notAdmitted:true})); await first;
  assert.equal(disabled,false);
  const retry=decide(true); assert.equal(calls,2);
  reject(new Error('outcome unknown')); await retry;
  await decide(false); assert.equal(calls,2); assert.equal(disabled,true);
  assert.deepEqual(pending,[true,false,true,false]);
  assert.deepEqual(reports.slice(0,3),['','busy','']);
  assert.match(reports[3],/outcome unknown.*Refresh request/);
});

test('request refresh coalesces clicks, fences decisions and requires fresh presentation', async () => {
 let resolve,reject,calls=0;
 const state=createInteraction({refreshCommand:{action:'inspect_interaction'},command:()=>{calls++;return new Promise((done,fail)=>{resolve=done;reject=fail})}});
 const first=state.refresh();await state.refresh();await state.decide({action:'approve'});
 assert.equal(calls,1);assert.equal(state.disabled,true);resolve();await first;
 assert.equal(state.refreshing,false);assert.equal(state.disabled,true);await state.decide({action:'approve'});assert.equal(calls,1);
 for(const locked of [false,true]){
  const next=createInteraction({refreshCommand:{},command:()=>new Promise((done,fail)=>{reject=fail})});next.locked=locked;
  const refreshing=next.refresh();reject(Error('offline'));await refreshing;
  assert.equal(next.disabled,locked);assert.match(next.error,/Pending-state read failed/);
 }
});

test('passive tickets keep pending and unknown decisions locked; explicit fresh reads release them', async () => {
  let reject, calls=0;
  const send=()=>{calls++;return new Promise((_,fail)=>{reject=fail})};
  const binding={command:send,refreshCommand:{action:'inspect_interaction'},ticket:'1'};
  const state=createInteraction(binding);
  const decision=state.decide({action:'approve'});
  state.update({...binding,ticket:'2'});
  assert(state.disabled);await state.decide({action:'approve'});assert.equal(calls,1);
  reject(Error('lost reply'));await decision;
  state.update({...binding,ticket:'3'});assert(state.disabled);
  state.update({...binding,command:async()=>{},ticket:'4'});
  await state.refresh();assert(state.disabled);
  state.update({...binding,ticket:'5'});assert.equal(state.disabled,false);
  const retry=state.decide({action:'approve'});assert.equal(calls,2);reject(Error('lost'));await retry;
  const data={pane:'main',generation:'1',kind:'approval',request:{id:'a',subject:{session_id:'s'}},ticket:'1'};
  assert.equal(interactionKey(data),interactionKey({...data,ticket:'2'}));
  assert.notEqual(interactionKey(data),interactionKey({...data,request:{...data.request,subject:{session_id:'child'}}}));
});

test('a failed repeated refresh cannot lend authority to a later passive ticket',async()=>{
  const binding={command:async()=>{},refreshCommand:{action:'inspect_interaction'},ticket:'1'};
  const state=createInteraction(binding);
  await state.decide({action:'approve'});
  await state.refresh();
  state.update({...binding,command:async()=>{throw Error('offline')}});
  await state.refresh();
  state.update({...binding,ticket:'2'});
  assert.equal(state.disabled,true);
  assert.match(state.error,/offline/);
});
