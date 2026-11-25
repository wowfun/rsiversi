import {clearDraftRecords} from './clear-draft-records.mjs';
import 'fake-indexeddb/auto';
import {test, beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {DraftStore, DraftEditor, captureOf} from '../drafts.js';

beforeEach(() => clearDraftRecords());
const open = () => DraftStore.open('a'.repeat(32), {kind:'local'});
const header = 'b'.repeat(64);

test('a workspace change carries text and images atomically and preserves a pending request', async () => {
  const store = await open();
  const image = {id:'f'.repeat(64), mime:'image/png', bytes:100, width:5, height:5};
  const original = await store.edit(await store.ensure('main','first',header), 'carry me', [image]);
  const destination = await store.ensure('main','second','c'.repeat(64));
  const [from, to] = await store.moveWorkspaceInput(original, destination);
  assert.equal(from.text, '');
  assert.deepEqual(from.images, []);
  assert.equal(to.text, 'carry me');
  assert.deepEqual(to.images, [image]);
  const frozen = await store.startDispatch(to, {kind:'message',id:'stable',opaque:'original request',text_bytes:8,images:1,captures:[]});
  await assert.rejects(store.moveWorkspaceInput(frozen, from), /pending/);
  assert.equal((await store.get('main','second')).pending.opaque, 'original request');
  assert.equal((await store.get('main','first')).text, '');
  await store.verify();
});

test('moving uses the input visible in the source window when a peer last saved other text', async () => {
  const store = await open();
  const from = new DraftEditor(store, await store.ensure('main','first',header));
  from.edit('my workspace draft');
  await from.flush();
  const peer = new DraftEditor(store, await store.get('main','first'));
  peer.edit('another window');
  await peer.flush();
  const to = await store.ensure('main','second',header);
  const [cleared, moved] = await store.moveWorkspaceInput(from.savedInput, to);
  from.adoptMoved(cleared);
  assert.equal(moved.text, 'my workspace draft');
  assert.equal(peer.text, 'another window');
  assert.equal((await store.get('main','first')).text, '');
  assert.equal((await store.get('main','second')).text, moved.text);
  await store.verify();
});

test('a workspace change leaves each captured reference bound to its own Session after reopen', async () => {
  const store = await open();
  const sourceRef = reference('source-capture');
  const targetRef = reference('target-capture');
  targetRef.metadata.target.session_id = 'destination';
  const image = {id:'e'.repeat(64), mime:'image/png', bytes:40, width:2, height:2};
  const source = await store.edit(await store.ensure('main','session',header), 'new text', [image], [sourceRef]);
  const target = await store.edit(await store.ensure('main','destination',header), 'old text', [], [targetRef]);
  await store.moveWorkspaceInput(source, target);
  const reopened = await open();
  const from = await reopened.get('main', 'session');
  const to = await reopened.get('main', 'destination');
  assert.deepEqual([from.text, from.images, from.references, from.releases], ['', [], [sourceRef], []]);
  assert.deepEqual([to.text, to.images, to.references, to.releases], ['new text', [image], [targetRef], []]);
  await reopened.verify();
});

test('an overfull destination keeps both workspace drafts intact', async () => {
  const store = await open();
  const image = {id:'d'.repeat(64), mime:'image/png', bytes:90, width:4, height:4};
  const from = await store.edit(await store.ensure('main','first',header), 'old', [image]);
  const to = await store.edit(await store.ensure('main','second',header), 'target', Array(8).fill(image));
  await assert.rejects(store.moveWorkspaceInput(from, to), /eight images/);
  assert.deepEqual(await store.get('main','first'), from);
  assert.deepEqual(await store.get('main','second'), to);
  await store.verify();
});

test('a maximum-size Session command survives reopening its submission journal', async () => {
  const store = await open();
  const task = 'a'.repeat(64 * 1024 - 2);
  const draft = await store.edit(await store.ensure('main','session',header), '/plan ' + task, []);
  const opaque = JSON.stringify({request:{kind:'command',invocation:{arguments:task}}});
  await store.startDispatch(draft,{kind:'command',id:'plan-request',opaque,text_bytes:0,images:0,captures:[]});
  const restored = await (await open()).get('main','session');
  assert.equal(restored.pending.opaque, opaque);
  assert.equal(restored.pending.id, 'plan-request');
});

test('primary key prefix returns only this endpoint, principal and pane in Session ID order', async () => {
  const local=await open();
  const other=await DraftStore.open('c'.repeat(32),{kind:'local'});
  const device=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
  for(const [store,pane,id] of [[local,'main','工作'],[local,'main','abc'],[local,'main-2','hidden'],[other,'main','hidden'],[device,'main','hidden']]) {
    await store.edit(await store.ensure(pane,id,header),id,[]);
  }
  assert.deepEqual((await local.list('main')).rows.map(record=>record.text),['abc','工作']);
});

test('definitive command rejection releases the intent and preserves its editable draft', async () => {
  const store = await open();
  const draft = await store.edit(await store.ensure('main','session',header),'/plan off',[]);
  const frozen = await store.startDispatch(draft,{kind:'command',id:'invalid',opaque:'original',text_bytes:0,images:0,captures:[]});
  const saved = await store.settle(frozen,{status:'rejected',error:'Attachments cannot accompany /plan off.'});
  assert.equal(saved.pending,null);
  assert.equal(saved.text,'/plan off');
  assert.equal((await store.edit(saved,'/plan describe',[])).text,'/plan describe');
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

test('ordinary Session drafts beyond 128 records and four MiB restore in bounded pages', async()=>{
  const store=await open(), ids=[];
  for(let i=0;i<129;i++) {
    const id=`draft-${String(i).padStart(3,'0')}`;
    await store.edit(await store.ensure('main',id,header),i<5?'x'.repeat(1024*1024):id,[]);
    ids.push(id);
  }
  let after;
  const found=[], reopened=await open();
  do {
    const page=await reopened.list('main',after);
    assert(page.rows.length<=32);
    found.push(...page.rows.map(row=>row.key[3]));
    after=page.next;
  } while(after);
  assert.deepEqual(found,ids);
  assert.equal((await reopened.get('main',ids[0])).text.length,1024*1024);
  assert.equal((await reopened.get('main',ids.at(-1))).text,ids.at(-1));
  await store.verify();
});

test('128 outstanding obligations across principals still allow saving ordinary text', async()=>{
  const store=await open();
  const other=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
  for(let i=0;i<128;i++) {
    const session=`fresh-${i}`;
    const owner=i%2 ? store : other, saved=await owner.ensure('main',session,header);
    if(i%2) await owner.startDispatch(saved,{kind:'message',id:session,opaque:'original '+session,text_bytes:0,images:0,captures:[]});
    else {
      const owned=reference(`owned-${i}`); owned.metadata.target.session_id=session;
      await owner.retireUnowned(saved,[captureOf(owned)]);
    }
  }
  const fresh=await store.ensure('main','overflow',header);
  const plain=await store.edit(fresh,'new',[]);
  const image={id:header,mime:'image/png',bytes:72,width:1,height:1};
  await assert.rejects(store.edit(plain,'new',[image]),/origin storage limit/);
  assert.deepEqual(await store.get('main','overflow'),plain);
  const first=await store.get('main','fresh-1');
  assert.equal(first.pending.opaque,'original fresh-1');
  await store.settle(first,{status:'complete'});
  assert.deepEqual((await store.edit(plain,'new',[image])).images,[image]);
  await store.verify();
});
function reference(id) {
  return {snapshot:{capture_id:id,sha256:'c'.repeat(64),byte_len:1},preview:'Reference',metadata:{
    target:{session_id:'session',header_sha256:header},source:{kind:'native',binding:{session_id:'source',header_sha256:header}},
    capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:header,scanned_bytes:9,omissions:[]}},text_bytes:9}};
}

test('empty visits use no quota and concurrent editors restore the last save',async()=>{
  const store=await open();
  for(let index=0;index<140;index++) await store.ensure('main',`visit-${index}`,header);
  assert.equal((await store.list('main')).rows.length,0);
  const first=await store.ensure('main','session',header),second=await store.ensure('main','session',header);
  const left=new DraftEditor(store,first),right=new DraftEditor(store,second);
  left.edit('first');await left.flush();
  right.edit('second');await right.flush();
  assert.equal(left.text,'first');assert.equal(right.text,'second');
  assert.equal((await store.ensure('main','session',header)).text,'second');
  left.edit('third');await left.flush();
  assert.equal(right.text,'second');
  const persisted=await store.get('main','session');
  assert.equal(persisted.text,'third');assert.equal(persisted.editRevision,3);
  await store.removeEmpty(await store.edit(persisted,'',[]));
  await assert.rejects(store.edit(first,'stale',[]),{code:'conflict'});
  await store.verify();
});

test('a later editor save preserves the frozen request and cannot revive a released reference',async()=>{
  const store=await open(), initial=await store.ensure('main','session',header);
  const ref=reference('shared');
  const left=await store.edit(initial,'before',[],[ref]);
  const request={kind:'message',id:'original',opaque:'{"seq":18446744073709551615}',text_bytes:6,images:0,captures:[captureOf(ref)]};
  const frozen=await store.startDispatch(left,request);
  const right=await store.edit(left,'new unsent text',[],[]);
  assert.deepEqual(right.pending,frozen.pending);
  const settled=await store.settle(frozen,{status:'complete'});
  assert.equal(settled.text,'new unsent text');
  assert.deepEqual(settled.releases,[captureOf(ref)]);
  await store.acknowledgeRelease(settled,captureOf(ref));
  await assert.rejects(store.edit(left,'stale reference',[],[ref]),/removed|retired/);
  assert.equal((await store.get('main','session')).text,'new unsent text');
  await store.verify();
});

test('sending from one window consumes its own text and preserves a later saved peer draft',async()=>{
  const store=await open();
  const left=new DraftEditor(store,await store.ensure('main','session',header));
  const right=new DraftEditor(store,await store.ensure('main','session',header));
  left.edit('send mine');await left.flush();
  right.edit('keep mine');await right.flush();
  const input=left.savedInput;
  assert.equal(input.text,'send mine');
  await left.update(()=>store.startDispatch(input,{kind:'message',id:'one',opaque:'send mine',text_bytes:9,images:0,captures:[]}));
  assert.equal(left.text,'send mine');assert.equal(left.savedInput.text,'send mine');
  await left.settle(left.record,{status:'unknown'});
  assert.equal(left.savedInput.text,'send mine');
  assert.equal(left.record.pending.opaque,'send mine');
  await left.settle(left.record,{status:'complete'});
  assert.equal(left.text,'');assert.equal(right.text,'keep mine');
  assert.equal((await store.get('main','session')).text,'keep mine');
  left.edit('next');await left.flush();
  assert.equal((await store.get('main','session')).text,'next');
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
