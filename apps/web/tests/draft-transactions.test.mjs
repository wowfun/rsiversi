import 'fake-indexeddb/auto';
import {test, beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {DraftStore, DraftEditor, captureOf} from '../drafts.js';

beforeEach(() => DraftStore.discardAllSaved());
const open = () => DraftStore.open('a'.repeat(32), {kind:'local'});
const header = 'b'.repeat(64);

test('more than 128 completed conversations reclaim empty dispatched records atomically', async()=>{
  const store=await open(); let first;
  for(let i=0;i<140;i++) {
    let saved=await store.ensure('main',`sent-${String(i).padStart(3,'0')}`,header);
    saved=await store.edit(saved,'hello',[]);
    saved=await store.freeze(saved,{kind:'message',id:`request-${i}`,opaque:'opaque',text_bytes:5,images:0,captures:[]});
    saved=await store.begin(saved);
    saved=await store.settle(saved,{status:'complete',receipt:'acknowledged'});
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
    if(i%6===2) await store.freeze(saved,{kind:'message',id:`pending-${i}`,opaque:'opaque',text_bytes:0,images:0,captures:[]});
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
  saved=await store.freeze(saved,{kind:'message',id:'request',opaque:'opaque',text_bytes:4,images:0,captures:refs.map(captureOf)});
  saved=await store.edit(saved,'',[],[]);
  assert.equal(saved.releases.length,12,'the pending request still owns four captures');
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
