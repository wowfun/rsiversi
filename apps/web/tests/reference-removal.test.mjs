import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../app.js', import.meta.url), 'utf8');
function element() {
  return {children:[], append(...items){this.children.push(...items);}, replaceChildren(){this.children=[];},
    setAttribute(name,value){this[name]=value;}};
}
test('reference removal edits the draft before cleanup and survives a cleanup refusal', async () => {
  let reject, focused = 0;
  const sent = [], connection = {request(method,payload){sent.push(JSON.parse(payload));return new Promise((_,fail)=>{reject=fail;});}};
  const scope = {element, connection, call:connection.request.bind(connection), boundCall:owner=>owner.request.bind(owner),
    button:(label,run)=>({label,run}), referenceOrigin:()=> 'Source', referenceInterval:()=>({through_seq:1,omissions:[]})};
  vm.runInNewContext(source.slice(source.indexOf('class Pane {'),source.indexOf('function basename('))+';this.Pane=Pane;',scope);
  const reference = {snapshot:{capture_id:'capture-original',sha256:'original'},metadata:{target:{session_id:'original-session',header_sha256:'header'}}}, editor = {text:'draft',images:[],references:[reference],referencesRevision:1};
  const pane = Object.assign(Object.create(scope.Pane.prototype), {index:'main',generation:'7',editor,
    addReference:{},searchHistory:{},addFile:{},referenceList:element(),draftStatus:element(),input:{focus(){focused++;}},
    renderComposer(){this.draftStatus.replaceChildren();this.renderReferenceCleanup();},
    edit(text,images,references){editor.references=references;}});
  pane.renderReferences();
  const removing = pane.referenceList.children[0].children.find(item=>item.label==='Remove reference').run();
  const removedBeforeReply = editor.references.length;
  await Promise.resolve();
  reject(new Error('connection unavailable')); await removing;
  assert.equal(removedBeforeReply,0);
  assert.equal(editor.references.length,0); assert.equal(editor.text,'draft'); assert.equal(focused,1);
  assert.equal(sent[0].operation.target.session_id,'original-session'); assert.equal(sent[0].generation,'7'); assert.equal(sent[0].operation.kind,'release');
  assert.match(pane.referenceCleanup.get('capture-original').message,/cleanup failed/i);
  pane.renderComposer();
  assert.equal(pane.draftStatus.children.length,1);
});

test('failed preview cleanup retains the exact capture for a coalesced retry', async () => {
  const calls=[]; let reject, resolve;
  const scope={};
  vm.runInNewContext(source.slice(source.indexOf('class Pane {'),source.indexOf('function basename('))+';this.Pane=Pane;',scope);
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
  vm.runInNewContext(source.slice(source.indexOf('class Pane {'),source.indexOf('function basename('))+';this.Pane=Pane;',scope);
  const pane=Object.assign(Object.create(scope.Pane.prototype),{index:'main',renderComposer(){}});
  const editor={record:{key:[...namespace,'main','original']},references:[]};
  const reference={snapshot:{capture_id:'capture'},metadata:{target:{session_id:'original',header_sha256:'header'}}};
  await assert.rejects(pane.releaseReference(reference,editor,'old',()=>{throw new Error('old transport');}),/offline/);
  const cleanup=[...pane.referenceCleanup.values()][0];
  scope.connection={drafts:{namespace:['different','local']},request:async()=>{throw new Error('must not send');}};
  await assert.rejects(cleanup.retry(),/original endpoint and account/);
  scope.connection={drafts:{namespace},request:async(_method,payload)=>calls.push(JSON.parse(payload))};
  await cleanup.retry();
  assert.equal(calls.length,1); assert.equal(calls[0].operation.target.session_id,'original');
  assert.equal(pane.referenceCleanup.size,0);
});


test('discard confirmation survives composer frames and resets with the pane', async () => {
  let discarded=0,reloaded=0;
  const scope={element,button:(label,run)=>({label,run}),connection:{storageNotice:'storage failed'},view:undefined,
    DraftStore:{async discardAllSaved(){discarded++;}},location:{reload(){reloaded++;}}};
  vm.runInNewContext(source.slice(source.indexOf('class Pane {'),source.indexOf('function basename('))+';this.Pane=Pane;',scope);
  const status=element(); status.removeAttribute=()=>{};
  const pane=Object.assign(Object.create(scope.Pane.prototype),{index:'main',generation:'1',input:{value:''},send:{},steer:{},deliveryMenu:{},draftStatus:status,
    renderImages(){},renderReferences(){},renderCommands(){},recovery:{replaceChildren(){}},render(){}});
  pane.renderComposer();
  await status.children.find(item=>item.label==='Discard saved drafts…').run();
  pane.renderComposer();
  assert.equal(discarded,0);
  await status.children.find(item=>item.label==='Confirm discard all saved drafts').run();
  assert.equal(discarded,1); assert.equal(reloaded,1);
  pane.reset(); assert.equal(pane.discardDraftsConfirmed,false);
});
