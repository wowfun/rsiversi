import {clearDraftRecords} from './clear-draft-records.mjs';
import 'fake-indexeddb/auto';
import {test, beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {DraftStore, DraftEditor, captureOf} from '../drafts.js';

beforeEach(() => clearDraftRecords());
const open = () => DraftStore.open('a'.repeat(32), {kind:'local'});
const header = 'b'.repeat(64);

test('primary key prefix returns only this endpoint, principal and pane in Session ID order', async () => {
  const local=await open();
  const other=await DraftStore.open('c'.repeat(32),{kind:'local'});
  const device=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
  for(const [store,pane,id] of [[local,'main','工作'],[local,'main','abc'],[local,'main-2','hidden'],[other,'main','hidden'],[device,'main','hidden']]) {
    await store.edit(await store.ensure(pane,id,header),id,[]);
  }
  assert.deepEqual((await local.list('main')).map(record=>record.text),['abc','工作']);
  assert.equal(local.database.transaction('drafts').objectStore('drafts').indexNames.length,0);
});

test('editing charges UTF-8 bytes and keeps the original frozen request and its caller-owned record', async () => {
  const store = await open();
  const opaque = 'request 中文 \ud800';
  const frozen = await store.startDispatch(await store.ensure('main','session',header),
    {kind:'message',id:'pending',opaque,text_bytes:8,images:0,captures:[]});
  const text = 'changed 😀 \udc00';
  await store.edit(frozen,text,[]);
  assert.equal(frozen.text,'');
  assert.equal((await store.get('main','session')).pending.opaque,opaque);
  const totals = await new Promise((resolve,reject) => {
    const read = store.database.transaction('usage').objectStore('usage').get('aggregate');
    read.onsuccess = () => resolve(read.result);
    read.onerror = () => reject(read.error);
  });
  assert.deepEqual(totals,[1,Buffer.byteLength(text),8,Buffer.byteLength(opaque)]);
});

test('only the first writer can claim a new dispatch or reclaim an explicitly unadmitted request', async () => {
  const left = await open(), right = await open();
  const draft = await left.edit(await left.ensure('main','session',header),'question',[]);
  const request = {kind:'command', id:'one', opaque:'original', text_bytes:8, images:0, captures:[]};
  const first = await Promise.allSettled([left.startDispatch(draft,request),right.startDispatch(draft,request)]);
  assert.deepEqual(first.map(result=>result.status).sort(),['fulfilled','rejected']);
  const recorded = await right.get('main','session');
  assert.equal(recorded.pending.phase,'dispatching');
  assert.equal(recorded.pendingRevision,draft.pendingRevision+1);
  await assert.rejects(right.claimPrepared(recorded),/already have executed/);
  const retry = await left.settle(recorded,{status:'not_admitted'});
  const competing = await Promise.allSettled([right.claimPrepared(retry),left.claimPrepared(retry)]);
  assert.deepEqual(competing.map(result=>result.status).sort(),['fulfilled','rejected']);
  const claimed = await left.get('main','session');
  assert.equal(claimed.pending.opaque,request.opaque);
  assert.equal(claimed.everDispatched,true);
  await left.verify();
});

test('rechecking the same result reads concurrent edits and rejects a changed frozen request or settlement', async () => {
  const left = await open(), right = await open();
  const creation = {session_id:'session',workspace_id:header,agent_preset_id:null};
  const draft = await left.ensure('main','session',header,creation);
  const sent = await left.startDispatch(draft,{kind:'message',id:'one',opaque:'{"seq":18446744073709551615}',text_bytes:0,images:0,captures:[]});
  assert.equal(sent.creation,null);
  assert.deepEqual(draft.creation,creation);
  for (const status of ['unknown','not_admitted']) {
    const first = await left.settle(await left.get('main','session'),{status});
    await right.edit(first,'typed while '+status,[]);
    const same = await left.settle(first,{status});
    assert.equal(same.pendingRevision,first.pendingRevision);
    assert.equal(same.text,'typed while '+status);
    const changed = {...same,pending:{...same.pending,opaque:'{"seq":18446744073709551614}'}};
    await assert.rejects(left.settle(changed,{status}),error=>error.code==='conflict');
  }
  const old = await left.get('main','session');
  const settled = await right.settle(old,{status:'complete'});
  await assert.rejects(left.settle(old,{status:'not_admitted'}),error=>error.code==='conflict' && error.saved.pendingRevision===settled.pendingRevision);
  const saved = await left.get('main','session');
  assert.equal(saved.pending,null);
  assert.equal(saved.text,'typed while not_admitted');
  assert(!Object.hasOwn(saved,'receipt'));
});

test('changing a draft preset rebinds the latest text, media and fork while fencing the old editor', async () => {
  const store = await open();
  const creation = {workspace_id:header, session_id:'session', agent_preset_id:'standard',
    fork:{source_session_id:'source', point:{kind:'through_turn',turn_id:'turn'}, turns:'all'}};
  const image = {bytes:128, width:4, height:4, mime:'image/png', id:'e'.repeat(64)};
  const saved = await store.edit(await store.ensure('main','session',header,creation),'first',[image]);
  const latest = await store.edit(saved,'newer input',[image]);
  const nextHeader = 'c'.repeat(64);
  const changed = await store.rebindPreset(saved,nextHeader,'editing');
  assert.equal(changed.header,nextHeader);
  assert.equal(changed.text,latest.text);
  assert.deepEqual(changed.images,[image]);
  assert.deepEqual(changed.creation,{...creation,agent_preset_id:'editing'});
  assert.notEqual(changed.incarnation,latest.incarnation);
  await assert.rejects(store.edit(latest,'old editor',[]),{code:'conflict'});
  const retained = reference('retained');
  retained.metadata.target.header_sha256 = nextHeader;
  const captured = await store.edit(changed,'with reference',[],[retained]);
  await assert.rejects(store.rebindPreset(captured,header,'standard'),/release references/);
  assert.deepEqual(await store.get('main','session'),captured);
  await store.verify();
});

test('more than 128 completed conversations reclaim empty dispatched records atomically', async()=>{
  const store=await open(); let first;
  for(let i=0;i<140;i++) {
    let saved=await store.ensure('main',`sent-${String(i).padStart(3,'0')}`,header);
    saved=await store.edit(saved,'hello',[]);
    saved=await store.startDispatch(saved,{kind:'message',id:`request-${i}`,opaque:'opaque',text_bytes:5,images:0,captures:[]});
    saved=await store.settle(saved,{status:'complete'});
    if(!i) first=saved;
  }
  assert.equal((await store.list('main')).length,128);
  await assert.rejects(store.edit(first,'stale editor input',[]),{code:'conflict'});
  await store.verify();
});

test('quota reclamation preserves unsent intent, pending writes, input and other principals', async()=>{
  const store=await open();
  const other=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
  for(let i=0;i<128;i++) {
    const session=`fresh-${i}`;
    if(i%6===5) {await other.edit(await other.ensure('main',session,header),'',[]);continue;}
    let saved=await store.ensure('main',session,header,i%6===0?{workspace_id:header,session_id:session,agent_preset_id:null}:undefined);
    const owned=reference(`owned-${i}`); owned.metadata.target.session_id=session;
    saved=await store.edit(saved,i%6===1?'keep':'',[],i%6===3?[owned]:[]);
    if(i%6===2) await store.startDispatch(saved,{kind:'message',id:`pending-${i}`,opaque:'opaque',text_bytes:0,images:0,captures:[]});
    if(i%6===4) await store.retireUnowned(saved,[captureOf(owned)]);
  }
  const fresh=await store.ensure('main','overflow',header);
  await assert.rejects(store.edit(fresh,'new',[]),/Saved drafts/);
  assert.equal(await store.get('main','overflow'),undefined);
  await store.verify();
});
function reference(id) {
  return {snapshot:{capture_id:id,sha256:'c'.repeat(64),byte_len:1},preview:'Reference',metadata:{
    target:{session_id:'session',header_sha256:header},source:{kind:'native',binding:{session_id:'source',header_sha256:header}},
    capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:header,scanned_bytes:9,omissions:[]}},text_bytes:9}};
}

test('opening more than 128 Sessions uses no draft quota; concurrent first writes conflict',async()=>{
  const store=await open();
  for(let index=0;index<140;index++) await store.ensure('main',`visit-${index}`,header);
  assert.equal((await store.list('main')).length,0);
  const first=await store.ensure('main','session',header),second=await store.ensure('main','session',header);
  let persisted=await store.edit(first,'first',[]);
  await assert.rejects(store.edit(second,'second',[]),{code:'conflict'});
  assert.equal((await store.get('main','session')).text,'first');
  persisted=await store.edit(persisted,'',[]); await store.removeEmpty(persisted);
  await assert.rejects(store.edit(first,'stale',[]),{code:'conflict'});
  await store.verify();
});

test('real draft transactions retire all owners at the 16-identity limit and roll back overflow',async()=>{
  const store=await open();let saved=await store.ensure('main','session',header);
  for(let index=0;index<12;index++) saved=await store.retireUnowned(saved,[captureOf(reference(`retired-${index}`))]);
  const refs=Array.from({length:4},(_,index)=>reference(`owned-${index}`));
  saved=await store.edit(saved,'send',[],refs);
  saved=await store.startDispatch(saved,{kind:'message',id:'request',opaque:'opaque',text_bytes:4,images:0,captures:refs.map(captureOf)});
  saved=await store.edit(saved,'',[],[]);
  assert.equal(saved.releases.length,12,'the pending request still owns four captures');
  saved=await store.settle(saved,{status:'not_admitted'});
  saved=await store.cancelPrepared(saved);
  assert.equal(saved.releases.length,16);
  await assert.rejects(store.retireUnowned(saved,[captureOf(reference('overflow'))]));
  assert.deepEqual(await store.get('main','session'),saved);
  await store.verify();
});

test('using saved input persists unsaved capture cleanup before dropping local ownership',async()=>{
  const store=await open();let saved=await store.ensure('main','session',header);
  saved=await store.edit(saved,'saved',[]);
  const editor=new DraftEditor(store,saved),original=store.edit.bind(store);
  store.edit=async()=>{throw Error('save failed')};
  editor.edit('local',[],[reference('unsaved')]);
  await assert.rejects(editor.flush(),/save failed/);
  store.edit=original;
  await editor.resolve(true);
  assert.equal(editor.text,'saved');assert.deepEqual(editor.references,[]);
  assert.deepEqual((await store.get('main','session')).releases,[captureOf(reference('unsaved'))]);
  await store.verify();
});

test('empty removal is idempotent but cannot delete a replaced incarnation',async()=>{
  const store=await open();let saved=await store.ensure('main','session',header);
  saved=await store.edit(saved,'',[]);
  await store.removeEmpty(saved);await store.removeEmpty(saved);
  const replacement=await store.ensure('main','session',header);
  await store.edit(replacement,'preserve',[]);
  await assert.rejects(store.removeEmpty(saved),{code:'conflict'});
  await store.verify();
});
test('confirmed nonempty removal refuses new edits and pending or reference obligations',async()=>{
  const store=await open();let saved=await store.edit(await store.ensure('main','session',header),'delete me',[]);
  const stale=saved;saved=await store.edit(saved,'newer',[]);
  await assert.rejects(store.removeSaved(stale),{code:'conflict'});
  saved=await store.startDispatch(saved,{kind:'message',id:'pending',opaque:'opaque',text_bytes:5,images:0,captures:[]});
  await assert.rejects(store.removeSaved(saved),/Resolve pending/);
  saved=await store.settle(saved,{status:'not_admitted'});
  saved=await store.cancelPrepared(saved);
  saved=await store.edit(saved,'keep until released',[],[reference('owned')]);
  await assert.rejects(store.removeSaved(saved),/release references/);
  saved=await store.edit(saved,'delete after cleanup',[],[]);
  await assert.rejects(store.removeSaved(saved),/release references/);
  saved=await store.acknowledgeRelease(saved,saved.releases[0]);
  await store.removeSaved(saved);assert.equal(await store.get('main','session'),undefined);
  await store.verify();
});

test('saved-input resolution fences edits until capture retirement is durable',async()=>{
  const store=await open();let saved=await store.ensure('main','session',header);
  saved=await store.edit(saved,'saved',[]);
  const editor=new DraftEditor(store,saved),write=store.edit.bind(store);
  store.edit=async()=>{throw Error('offline')};
  editor.edit('unsaved',[],[reference('retiring')]);await assert.rejects(editor.flush());
  store.edit=write;
  const retire=store.retireUnowned.bind(store);let release,entered;
  const gate=new Promise(resolve=>{release=resolve}),waiting=new Promise(resolve=>{entered=resolve});
  store.retireUnowned=async(...args)=>{entered();await gate;return retire(...args)};
  const resolving=editor.resolve(true);await waiting;
  assert.throws(()=>editor.edit('racing',[],[reference('retiring')]),/conflict resolution/);
  release();await resolving;
  assert.equal(editor.text,'saved');assert.equal(editor.resolving,false);
  assert.deepEqual(editor.references,[]);
  assert.deepEqual(editor.record.releases,[captureOf(reference('retiring'))]);
});

test('failed saved-input resolution cannot partially release still-local captures',async()=>{
  const store=await open();let saved=await store.ensure('main','session',header);
  for(let index=0;index<13;index++) saved=await store.retireUnowned(saved,[captureOf(reference(`old-${index}`))]);
  const editor=new DraftEditor(store,saved),write=store.edit.bind(store);
  store.edit=async()=>{throw Error('offline')};
  editor.edit('unsaved',[],[reference('local-a'),reference('local-b')]);
  await assert.rejects(editor.flush());store.edit=write;
  for(let index=13;index<15;index++) saved=await store.retireUnowned(saved,[captureOf(reference(`old-${index}`))]);
  await assert.rejects(editor.resolve(true));
  assert.equal(editor.text,'unsaved');assert.equal(editor.references.length,2);
  assert.deepEqual((await store.get('main','session')).releases,saved.releases);
});
