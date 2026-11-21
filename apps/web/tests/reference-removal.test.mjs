import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {captureKey, captureOf} from '../drafts.js';

test('cleanup cannot switch endpoints while reloading an evicted editor',async()=>{
  const namespace=['endpoint','local'], calls=[];
  const original={drafts:{namespace}};
  const scope={connection:original,boundCall:connection=>async()=>{calls.push(connection);throw Error('offline')},DraftEditor:class {constructor(store,record){Object.assign(this,{store,record,references:[]})}}};
  const Pane=createPaneClass(scope),pane=new Pane('main'); pane.renderComposer=()=>{};
  let finish;
  const record={key:[...namespace,'main','saved'],references:[],releases:[]};
  const editor={record,references:[],store:{get:()=>new Promise(resolve=>finish=resolve)}};
  const reference={metadata:{target:{session_id:'saved'}},snapshot:{capture_id:'retired'}};
  await assert.rejects(pane.releaseReference(reference,editor,'one'),/offline/);
  const cleanup=[...pane.referenceCleanup.values()][0]; cleanup.editor={deref:()=>undefined};
  const retry=cleanup.retry(); await new Promise(setImmediate);
  scope.connection={drafts:{namespace:['different','local']}};finish(record);
  await assert.rejects(retry,/connection|Reconnect/i);
  assert.deepEqual(calls,[original]);
});

test('reference removal uses full identities and refuses mutation during submission', async()=>{
  const target={session_id:'saved',header_sha256:'header'};
  const references=['first','second'].map(sha256=>({metadata:{target},snapshot:{capture_id:'same',sha256,byte_len:1}}));
  const released=[];
  const Pane=createPaneClass({referenceInterval:()=>({through_seq:1,omissions:[]}),referenceOrigin:()=> 'Saved',
    boundCall:()=>async(_method,payload)=>released.push(JSON.parse(payload).operation.snapshot.sha256)});
  const pane=new Pane('main');
  const editor={text:'keep',images:[],references,record:{references,releases:[]},
    async flush(){this.record.references=this.references;},
    store:{async retireUnowned(record,[capture]){return {...record,releases:[capture]};},
      async acknowledgeRelease(record){return {...record,releases:[]};}},
    async update(operation){this.record=await operation(this.record);}};
  Object.assign(pane,{editor,generation:'one',input:{focus(){}},
    edit(_text,_images,references){editor.references=references;},showReferencePicker(reference){this.previewed=reference;}});
  pane.renderComposer();
  assert.deepEqual(pane.composerView.references.map(row=>row.id),references.map(item=>captureKey(captureOf(item))));
  const id=captureKey(captureOf(references[1]));
  pane.previewReference(id);assert.equal(pane.previewed,references[1]);
  pane.submitting='Sending message';
  await assert.rejects(pane.removeReference(id),/current operation/);
  assert.equal(editor.references.length,2);
  pane.submitting=undefined;
  pane.referenceState(editor).preparing=[captureOf(references[1])];
  await assert.rejects(pane.removeReference(id),/current operation/);
  pane.referenceState(editor).preparing=undefined;
  await pane.removeReference(id);
  assert.deepEqual(editor.references,[references[0]]);
  assert.deepEqual(released,['second']);
});

test('explicit cleanup during an automatic drain retries previously failed captures', async()=>{
  const captures=['bad','good'].map(capture_id=>({target:{session_id:'saved'},snapshot:{capture_id}}));
  const calls=[];let completeGood;
  const Pane=createPaneClass({boundCall:()=>async(_method,payload)=>{
    const id=JSON.parse(payload).operation.snapshot.capture_id;calls.push(id);
    if(calls.length===1)throw Error('offline');
    if(id==='good')await new Promise(resolve=>{completeGood=resolve;});
  }});
  const pane=new Pane('main');
  const editor={references:[],record:{references:[],releases:captures},async flush(){},
    store:{async acknowledgeRelease(record,capture){return {...record,releases:record.releases.filter(item=>item!==capture)};}},
    async update(operation){this.record=await operation(this.record);}};
  await assert.rejects(pane.drainReferenceReleases({...editor,record:{...editor.record,releases:[captures[0]]}},'one'),/offline/);
  const automatic=pane.drainReferenceReleases(editor,'one');
  await new Promise(setImmediate);
  const retry=pane.drainReferenceReleases(editor,'one',true);
  await new Promise(setImmediate);completeGood();
  await Promise.all([automatic,retry]);
  assert.deepEqual(calls,['bad','good','bad']);
  assert.deepEqual(editor.record.releases,[]);
});

test('cleanup distinguishes full capture identities and authenticated namespaces',async()=>{
  const calls=[],namespace=['endpoint','local'];
  const scope={connection:{drafts:{namespace}},boundCall:()=>async(_method,payload)=>calls.push(JSON.parse(payload).operation)};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  const target={session_id:'saved',header_sha256:'header'}, snapshot={capture_id:'same',sha256:'first',byte_len:1};
  const first={metadata:{target},snapshot},second={metadata:{target},snapshot:{...snapshot,sha256:'second'}};
  const editor={record:{key:[...namespace,'main','saved'],references:[first],pending:null},references:[first]};
  await pane.releaseReference(second,editor,'one');
  assert.equal(calls.length,1,'another capture with the same id must not block cleanup');
  assert.equal(calls[0].snapshot.sha256,'second');
  editor.references=[];editor.record.references=[];
  await Promise.all([pane.releaseReference(first,editor,'one'),pane.releaseReference(second,editor,'one')]);
  assert.deepEqual(calls.slice(1).map(call=>call.snapshot.sha256).sort(),['first','second']);
  scope.boundCall=()=>async()=>{throw Error('offline')};
  await assert.rejects(pane.releaseReference(first,editor,'one'),/offline/);
  const nextNamespace=['another-endpoint','local'];
  scope.connection={drafts:{namespace:nextNamespace}};
  const nextEditor={...editor,record:{...editor.record,key:[...nextNamespace,'main','saved']}};
  await assert.rejects(pane.releaseReference(first,nextEditor,'two'),/offline/);
  assert.equal(pane.referenceCleanup.size,2,'different endpoints must retain independent cleanup');
});

test('failed durable release is not retried by editor changes; explicit retry remains available', async()=>{
  const namespace=['endpoint','local'], capture={target:{session_id:'saved'},snapshot:{capture_id:'retired'}};
  const record={key:[...namespace,'main','saved'],header:'header',text:'',images:[],references:[],pending:null,releases:[capture]};
  let calls=0,offline=true;
  const store={namespace,key:()=>record.key,ensure:async()=>record,acknowledgeRelease:async record=>({...record,releases:[]})};
  class Editor {
    constructor(store,record){Object.assign(this,{store,record,text:'',images:[],references:[]});}
    async flush() {}
    async update(operation){this.record=await operation(this.record);this.changed();}
  }
  const scope={connection:{drafts:store},DraftEditor:Editor,boundCall:()=>async()=>{calls++;if(offline)throw Error('offline')}};
  const Pane=createPaneClass(scope),pane=new Pane('main');pane.generation='one';
  await pane.bindDraft({session:'saved',header:'header'});
  await new Promise(setImmediate);
  for(let i=0;i<5;i++){pane.editor.changed();await new Promise(setImmediate);}
  assert.equal(calls,1,'typing repeatedly retried the same failed network operation');
  offline=false;
  await pane.composerView.cleanups[0].retry();
  assert.equal(calls,2);
  assert.deepEqual(pane.editor.record.releases,[]);
});

test('failed persistence and pending ownership prevent remote reference release', async () => {
  const calls=[];
  const scope={boundCall:()=>async(...args)=>calls.push(args)};
  const Pane=createPaneClass(scope);
  const reference={snapshot:{capture_id:'owned'},metadata:{target:{session_id:'session'}}};
  const editor={text:'draft',images:[],references:[reference],record:{references:[reference],pending:null},async flush(){throw Error('disk full')}};
  const pane=Object.assign(Object.create(Pane.prototype),{index:'main',generation:'one',editor,input:{focus(){}},renderComposer(){},edit(_text,_images,references){editor.references=references}});
  await assert.rejects(pane.removeReference(captureKey(captureOf(reference))),/disk full/);
  await pane.releaseReference(reference,editor,'one');
  assert.equal(calls.length,0);
  editor.record.references=[];editor.record.pending={captures:[{snapshot:reference.snapshot,target:reference.metadata.target}]};
  await pane.releaseReference(reference,editor,'one');
  assert.equal(calls.length,0);
  editor.record.pending=null;
  pane.referenceState(editor).preparing=[{snapshot:reference.snapshot,target:reference.metadata.target}];
  await pane.releaseReference(reference,editor,'one');
  assert.equal(calls.length,0);
  pane.referenceState(editor).preparing=undefined;
  await pane.releaseReference(reference,editor,'one');
  assert.equal(calls.length,1);
});

test('reference removal retains focus and persists retirement before remote release',async()=>{
  const events=[], namespace=['endpoint','local'];
  const reference={snapshot:{capture_id:'owned'},metadata:{target:{session_id:'session'}}};
  const scope={connection:{drafts:{namespace}},boundCall:()=>async()=>events.push('release')};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  const editor={text:'keep text',images:[],references:[reference],
    record:{key:[...namespace,'main','session'],references:[reference],releases:[]},
    async flush(){events.push('saved');this.record.references=[];},
    store:{async retireUnowned(record,[capture]){events.push('retired');return {...record,releases:[capture]};},
      async acknowledgeRelease(record){events.push('acknowledged');return {...record,releases:[]};}},
    async update(operation){this.record=await operation(this.record);}};
  Object.assign(pane,{generation:'one',editor,input:{focus(){events.push('focus')}},
    edit(text,images,references){assert.equal(text,'keep text');events.push('edit');editor.references=references;}});
  await pane.removeReference(captureKey(captureOf(reference)));
  assert.deepEqual(events,['edit','focus','saved','retired','release','acknowledged']);
});

test('coalesced removal retains cleanup ownership when the release outbox is full',async()=>{
  const namespace=['endpoint','local'],calls=[];
  const reference={snapshot:{capture_id:'coalesced'},metadata:{target:{session_id:'session'}}};
  const scope={connection:{drafts:{namespace}},boundCall:()=>async(_method,payload)=>calls.push(JSON.parse(payload))};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  const editor={text:'keep',images:[],references:[reference],record:{key:[...namespace,'main','session'],references:[],releases:[]},
    async flush(){},store:{async retireUnowned(){throw Error('outbox full')}},async update(operation){this.record=await operation(this.record)}};
  Object.assign(pane,{generation:'one',editor,input:{focus(){}},renderComposer(){},edit(_text,_images,refs){editor.references=refs}});
  await assert.rejects(pane.removeReference(captureKey(captureOf(reference))),/outbox full/);
  assert.deepEqual(editor.references,[]);assert.deepEqual(editor.record.releases,[]);
  assert.equal(pane.referenceCleanup.size,1);assert.equal(calls.length,0);
  await [...pane.referenceCleanup.values()][0].retry();
  assert.equal(calls[0].operation.snapshot.capture_id,'coalesced');
  assert.equal(pane.referenceCleanup.size,0);
});

test('failed preview cleanup retains the exact capture for a coalesced retry', async () => {
  const calls=[]; let reject, resolve;
  const scope={};
  scope.Pane=createPaneClass(scope);
  const pane=Object.assign(Object.create(scope.Pane.prototype),{index:'main',renderComposer(){}});
  const call=(_method,payload)=>{calls.push(JSON.parse(payload));return new Promise((ok,fail)=>{resolve=ok;reject=fail;});};
  scope.boundCall=()=>call;
  const reference={snapshot:{capture_id:'exact-capture'},metadata:{target:{session_id:'original-session',header_sha256:'header'}}}, editor={references:[]};
  const release=()=>pane.releaseReference(reference,editor,'one');
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
  await assert.rejects(pane.releaseReference(reference,editor,'old'),/offline/);
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
    DraftStore:{async discardAllDrafts(){discarded++;}},location:{reload(){reloaded++;}}};
  const Pane=createPaneClass(scope);
  const pane=Object.assign(new Pane('main'),{generation:'1',input:{value:''},commands:{},recovery:{replaceChildren(){}},render(){}});
  pane.renderComposer();
  await pane.discardSavedDrafts();pane.renderComposer();
  assert.equal(discarded,0);assert.equal(pane.composerView.discardConfirmed,true);
  await pane.discardSavedDrafts();
  assert.equal(discarded,1);assert.equal(reloaded,1);
  pane.reset();assert.equal(pane.discardDraftsConfirmed,false);
});

test('visible durable cleanup retry also acknowledges its release obligation', async()=>{
  const namespace=['endpoint','local'], capture={target:{session_id:'saved'},snapshot:{capture_id:'retired'}};
  let offline=true, calls=0;
  const scope={connection:{drafts:{namespace}},boundCall:()=>async()=>{calls++;if(offline)throw Error('offline')}};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  pane.generation='one';
  const editor={text:'',images:[],references:[],record:{key:[...namespace,'main','saved'],references:[],pending:null,releases:[capture]},
    async flush(){ if(this.failure) throw this.failure; this.dirty=false; },
    store:{acknowledgeRelease:async record=>({...record,releases:[]})},async update(operation){this.record=await operation(this.record)}};
  pane.editor=editor;
  await assert.rejects(pane.drainReferenceReleases(editor,'one'),/offline/);
  assert.equal(editor.record.releases.length,1);
  offline=false;
  editor.dirty=true;
  await pane.composerView.cleanups[0].retry();
  assert.equal(calls,2);
  assert.deepEqual(editor.record.releases,[]);
});

test('cleanup can recover an evicted editor without retaining its text', async()=>{
  const namespace=['endpoint','local'],capture={target:{session_id:'saved'},snapshot:{capture_id:'retired'}};
  let record={key:[...namespace,'main','saved'],text:'large draft',references:[],pending:null,releases:[capture]};
  let offline=true,calls=0,reads=0;
  const store={namespace,async get(pane,session){assert.equal(pane,'main');assert.equal(session,'saved');reads++;return record},
    async acknowledgeRelease(saved){record={...saved,releases:[]};return record}};
  class Editor {
    constructor(store,record){Object.assign(this,{store,record,references:record.references});}
    async flush() {}
    async update(operation){this.record=await operation(this.record);}
  }
  const scope={connection:{drafts:store},DraftEditor:Editor,boundCall:()=>async()=>{calls++;if(offline)throw Error('offline')}};
  const Pane=createPaneClass(scope),pane=new Pane('main');pane.generation='one';
  const editor=new Editor(store,record);pane.editor=editor;
  await assert.rejects(pane.drainReferenceReleases(editor,'one'),/offline/);
  const cleanup=[...pane.referenceCleanup.values()][0];
  assert.ok(cleanup.editor instanceof WeakRef);
  // Exercise collected-editor recovery deterministically; no forced GC timing.
  cleanup.editor={deref:()=>undefined};
  offline=false;
  await pane.composerView.cleanups[0].retry();
  assert.ok(reads>0);assert.equal(calls,2);assert.deepEqual(record.releases,[]);
});

test('explicit cleanup reports conflicts and busy ownership; one failed capture does not block another', async()=>{
  const namespace=['endpoint','local'],calls=[];
  const captures=['bad','good'].map(id=>({target:{session_id:'saved'},snapshot:{capture_id:id}}));
  const scope={connection:{drafts:{namespace}},boundCall:()=>async(_method,payload)=>{
    const id=JSON.parse(payload).operation.snapshot.capture_id; calls.push(id);
    if(id==='bad')throw Error('unavailable capture');
  }};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  const editor={references:[],record:{key:[...namespace,'main','saved'],references:[],releases:captures},
    async flush(){if(this.failure)throw this.failure;this.dirty=false;},
    store:{async acknowledgeRelease(record,capture){return {...record,releases:record.releases.filter(item=>item!==capture)};}},
    async update(operation){this.record=await operation(this.record);}};
  editor.failure=Error('resolve saved input conflict');
  await assert.rejects(pane.drainReferenceReleases(editor,'one',true),/resolve saved input conflict/);
  editor.failure=undefined;
  pane.referenceState(editor).holdCleanup=1;
  await assert.rejects(pane.drainReferenceReleases(editor,'one',true),/reference preparation/);
  assert.deepEqual(calls,[]);
  pane.referenceState(editor).holdCleanup=0;
  editor.dirty=true;
  await assert.rejects(pane.drainReferenceReleases(editor,'one',true),/unavailable capture/);
  assert.deepEqual(calls,['bad','good']);
  assert.deepEqual(editor.record.releases,[captures[0]]);
});
