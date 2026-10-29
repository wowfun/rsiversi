import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';

test('reference removal edits the draft before cleanup and survives a cleanup refusal', async () => {
  let reject, focused = 0;
  const sent = [], connection = {request(method,payload){sent.push(JSON.parse(payload));return new Promise((_,fail)=>{reject=fail;});}};
  const scope = {connection, call:connection.request.bind(connection), boundCall:owner=>owner.request.bind(owner),
    referenceOrigin:()=> 'Source', referenceInterval:()=>({through_seq:1,omissions:[]})};
  scope.Pane=createPaneClass(scope);
  const reference = {snapshot:{capture_id:'capture-original',sha256:'original'},metadata:{target:{session_id:'original-session',header_sha256:'header'}}}, editor = {record:{},text:'draft',images:[],references:[reference],referencesRevision:1};
  const pane = Object.assign(new scope.Pane('main'), {index:'main',generation:'7',editor,
    commands:{},input:{focus(){focused++;}},
    edit(text,images,references){editor.references=references;}});
  const removing = pane.removeReference('capture-original');
  const removedBeforeReply = editor.references.length;
  await Promise.resolve();
  reject(new Error('connection unavailable')); await removing;
  assert.equal(removedBeforeReply,0);
  assert.equal(editor.references.length,0); assert.equal(editor.text,'draft'); assert.equal(focused,1);
  assert.equal(sent[0].operation.target.session_id,'original-session'); assert.equal(sent[0].generation,'7'); assert.equal(sent[0].operation.kind,'release');
  assert.match(pane.referenceCleanup.get('capture-original').message,/cleanup failed/i);
  pane.renderComposer();
  assert.equal(pane.composerView.cleanups.length,1);
});

test('failed preview cleanup retains the exact capture for a coalesced retry', async () => {
  const calls=[]; let reject, resolve;
  const scope={};
  scope.Pane=createPaneClass(scope);
  const pane=Object.assign(Object.create(scope.Pane.prototype),{index:'main',renderComposer(){}});
  const call=(_method,payload)=>{calls.push(JSON.parse(payload));return new Promise((ok,fail)=>{resolve=ok;reject=fail;});};
  const reference={snapshot:{capture_id:'exact-capture'},metadata:{target:{session_id:'original-session',header_sha256:'header'}}}, editor={references:[]};
  const release=()=>pane.releaseReference(reference,editor,'one',call);
  const first=release(); const duplicate=release(); await Promise.resolve();
  assert.equal(calls.length,1);
  reject(new Error('offline')); await assert.rejects(first,/Reference cleanup failed/); await assert.rejects(duplicate,/Reference cleanup failed/);
  assert.equal(pane.referenceCleanup.size,1);
  const retry=[...pane.referenceCleanup.values()][0].retry(); await Promise.resolve();
  assert.equal(calls.length,2); assert.equal(calls[1].operation.snapshot.capture_id,'exact-capture');
  resolve(); await retry; assert.equal(pane.referenceCleanup.size,0);
  editor.references.push(reference); await release(); assert.equal(calls.length,2);
});

test('cleanup retry uses a fresh connection only for the original endpoint and principal', async () => {
  const namespace=['endpoint','local'], calls=[];
  const scope={connection:{drafts:{namespace},request:async()=>{throw new Error('offline');}},boundCall:owner=>owner.request.bind(owner)};
  scope.Pane=createPaneClass(scope);
  const pane=Object.assign(Object.create(scope.Pane.prototype),{index:'main',renderComposer(){}});
  const editor={record:{key:[...namespace,'main','original']},references:[]};
  const reference={snapshot:{capture_id:'capture'},metadata:{target:{session_id:'original',header_sha256:'header'}}};
  await assert.rejects(pane.releaseReference(reference,editor,'old',()=>{throw new Error('old transport');}),/offline/);
  const cleanup=[...pane.referenceCleanup.values()][0];
  scope.connection={drafts:{namespace:['different','local']},request:async()=>{throw new Error('must not send');}};
  await assert.rejects(cleanup.retry(),/original endpoint and device/);
  scope.connection={drafts:{namespace},request:async(_method,payload)=>calls.push(JSON.parse(payload))};
  await cleanup.retry();
  assert.equal(calls.length,1); assert.equal(calls[0].operation.target.session_id,'original');
  assert.equal(pane.referenceCleanup.size,0);
});


test('discard confirmation survives composer frames and resets with the pane', async () => {
  let discarded=0,reloaded=0;
  const scope={connection:{storageNotice:'storage failed'},view:undefined,
    DraftStore:{async discardAllSaved(){discarded++;}},location:{reload(){reloaded++;}}};
  const Pane=createPaneClass(scope);
  const pane=Object.assign(new Pane('main'),{generation:'1',input:{value:''},commands:{},recovery:{replaceChildren(){}},render(){}});
  pane.renderComposer();
  await pane.discardSavedDrafts();pane.renderComposer();
  assert.equal(discarded,0);assert.equal(pane.composerView.discardConfirmed,true);
  await pane.discardSavedDrafts();
  assert.equal(discarded,1);assert.equal(reloaded,1);
  pane.reset();assert.equal(pane.discardDraftsConfirmed,false);
});
