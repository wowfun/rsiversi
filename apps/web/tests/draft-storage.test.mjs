import assert from 'node:assert/strict';
import {test} from 'node:test';
import {DraftStore,validateEditor} from '../drafts.js';

test('fallback blank saves after a transient read failure without overwriting a concurrent draft', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  await assert.rejects(store.ensure('main','session','b'.repeat(64)), /unavailable/);
  const blank = store.blank('main','session','b'.repeat(64));
  let persisted;
  store.mutate = async (_key, change) => persisted = change(persisted && structuredClone(persisted));
  const saved = await store.edit(blank,'Recovered input',[]);
  assert.equal(saved.text,'Recovered input');
  const concurrent = store.blank('main','session','b'.repeat(64));
  await assert.rejects(store.edit(concurrent,'Overwrite',[]), /another editor/);
  assert.equal(persisted.text,'Recovered input');
});
test('unavailable storage preserves a usable blank editor and explains failed operations', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  const blank = store.blank('main', 'session', 'b'.repeat(64));
  assert.equal(blank.text, '');
  for (const operation of [() => store.get('main', 'session'), () => store.list('main'), () => store.ensure('main','session','b'.repeat(64))]) {
    await assert.rejects(operation, /Draft storage is unavailable/);
  }
});

test('frozen references retain independent capture identities at the durable draft boundary', () => {
  const reference = {snapshot:{capture_id:'capture-a',sha256:'a'.repeat(64),byte_len:900},preview:'Reference',metadata:{
    source:{kind:'native',binding:{session_id:'source',header_sha256:'b'.repeat(64)}},target:{session_id:'target',header_sha256:'c'.repeat(64)},
    capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:'d'.repeat(64),scanned_bytes:100,omissions:[]}},text_bytes:9}};
  assert.ok(validateEditor('',[],[reference])>0);
  const duplicate=structuredClone(reference);duplicate.snapshot.capture_id='capture-b';
  assert.ok(validateEditor('',[],[reference,duplicate])>0);
  for (const capture of [undefined,'','x'.repeat(65)]) {
    const invalid=structuredClone(reference);
    if(capture===undefined)delete invalid.snapshot.capture_id;else invalid.snapshot.capture_id=capture;
    assert.throws(()=>validateEditor('',[],[invalid]), /Invalid frozen/);
  }
});

test('explicit discard removes both draft databases and reports blocked deletion', async () => {
  const original = globalThis.indexedDB;
  const requests = new Map();
  globalThis.indexedDB = {deleteDatabase(name){ const request={}; requests.set(name,request); return request; }};
  try {
    const discard = DraftStore.discardAllDrafts();
    await Promise.resolve();
    assert.deepEqual([...requests.keys()], ['rsi.composer','rsi.composer-starts']);
    for (const request of requests.values()) request.onsuccess(); await discard;
    const blocked = DraftStore.discardAllDrafts();
    await Promise.resolve();
    requests.get('rsi.composer').onsuccess(); requests.get('rsi.composer-starts').onblocked();
    await assert.rejects(blocked, /Close other RSIversi tabs/);
  } finally { globalThis.indexedDB = original; }
});

test('command controls retain editor content through unknown-result recovery and completion', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  let saved = store.blank('main','session','b'.repeat(64));
  saved.text = 'Unsubmitted work';
  store.mutate = async (_key, change) => saved = change(structuredClone(saved));
  saved = await store.startDispatch(saved, {kind:'command', id:'command-1',opaque:'frozen',text_bytes:0,images:0,captures:[]}, true);
  saved = await store.settle(saved, {status:'unknown'});
  assert.equal(saved.pending.opaque, 'frozen');
  assert.equal(saved.pending.preserve_editor, true);
  saved = await store.settle(saved, {status:'complete'});
  assert.equal(saved.text, 'Unsubmitted work');
  assert.equal(saved.pending, null);
  assert.throws(() => store.startDispatch(saved,{kind:'message'},true), /Only a command/);
});
test('independent fork drafts retain their exact creation intent through storage and replacement', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  const blank = (id, turns, point = {kind:'through_turn',turn_id:'finished-turn'}) => store.blank('main', id, 'b'.repeat(64), {
    workspace_id:'c'.repeat(64), session_id:id, agent_preset_id:'dsh-fixture',
    fork:{source_session_id:'source',point,turns},
  });
  for (const selection of ['all', 'none', {last:2}]) {
    const previous=blank('first',selection); previous.text='Review the earlier work.';
    const next=blank('second',selection);
    store.mutateMany=async(_keys,change)=>change([structuredClone(previous),structuredClone(next)]);
    const moved=await store.moveFresh(previous,next);
    assert.equal(moved.text,previous.text);
    assert.deepEqual(moved.creation.fork,previous.creation.fork);
  }
  for(const invalid of [{last:0},{last:1,other:true},'some',-1]) assert.throws(()=>blank('first',invalid));
  assert.throws(()=>blank('source','all'));
  const old=blank('first','all'), different=blank('replacement','none');
  store.mutateMany=async(_keys,change)=>change([old,different]);
  await assert.rejects(store.moveFresh(old,different));
  const point={kind:'before_turn',turn_id:'accepted-turn'};
  const before=blank('before','all',point),same=blank('same','all',point);
  store.mutateMany=async(_keys,change)=>change([before,same]);
  assert.equal((await store.moveFresh(before,same)).creation.fork.point.kind,'before_turn');
  const through=blank('through','all');
  store.mutateMany=async(_keys,change)=>change([before,through]);
  await assert.rejects(store.moveFresh(before,through),/selected history/);
  for(const invalid of [{kind:'through_turn',turn_id:''},{kind:'inside_turn',turn_id:'turn'},
    {kind:'before_turn',turn_id:'turn',other:true}])assert.throws(()=>blank('invalid','all',invalid));
});

test('capture identity is independent of JSON object member ordering', async()=>{
  const store=new DraftStore(undefined,'a'.repeat(32),'local');
  let saved=store.blank('main','session','b'.repeat(64));
  const snapshot={capture_id:'identity',sha256:'c'.repeat(64),byte_len:1},target={session_id:'session',header_sha256:saved.header};
  saved.references=[{metadata:{target},snapshot}];
  store.mutate=async(_key,change)=>saved=change(structuredClone(saved));
  const capture={snapshot:{byte_len:1,sha256:snapshot.sha256,capture_id:'identity'},target:{header_sha256:saved.header,session_id:'session'}};
  saved=await store.startDispatch(saved,{kind:'message',id:'request',opaque:'opaque',text_bytes:0,images:0,captures:[capture]});
  assert.equal(saved.pending.captures[0].snapshot.capture_id,'identity');
  saved.references=[];saved.pending=null;saved.releases=[capture];
  saved=await store.acknowledgeRelease(saved,{target,snapshot});
  assert.deepEqual(saved.releases,[]);
});

test('retiring an owned or queued capture needs no transaction, while an unseen capture rechecks durable ownership', async()=>{
  const store=new DraftStore(undefined,'a'.repeat(32),'local');
  const record=store.blank('main','session','b'.repeat(64));
  const capture={target:{session_id:'session',header_sha256:record.header},snapshot:{capture_id:'capture',sha256:'c'.repeat(64),byte_len:1}};
  let writes=0, saved=structuredClone(record);
  store.mutate=async(_key,change)=>{writes++;return saved=change(structuredClone(saved));};
  for (const owner of ['references','pending','releases']) {
    const expected=structuredClone(record);
    if(owner==='references')expected.references=[{metadata:{target:capture.target},snapshot:capture.snapshot}];
    else if(owner==='pending')expected.pending={captures:[capture]};
    else expected.releases=[capture];
    saved=structuredClone(expected);
    assert.deepEqual(await store.retireUnowned(expected,[capture]),expected);
    assert.equal(writes,0);
  }
  saved=structuredClone(record);saved.pending={captures:[capture]};
  assert.deepEqual((await store.retireUnowned(record,[capture])).releases,[]);
  assert.equal(writes,1);
  saved.pending=null;
  assert.deepEqual((await store.retireUnowned(record,[capture])).releases,[capture]);
  assert.equal(writes,2);
});

test('command trigger consumption survives recovery and never deletes newer text', async()=>{
  for(const changed of [false,true]) {
    const store=new DraftStore(undefined,'a'.repeat(32),'local');
    let saved=store.blank('main','command-span','b'.repeat(64));saved.text='before /help after';
    store.mutate=async(_key,change)=>saved=change(structuredClone(saved));
    saved=await store.startDispatch(saved,{kind:'command',id:'help',opaque:'opaque',text_bytes:0,images:0,captures:[]},true,{start:7,end:12});
    saved=await store.settle(saved,{status:'unknown'});
    assert.equal(saved.text,'before /help after');
    if(changed)saved=await store.edit(saved,'new text',[]);
    saved=await store.settle(saved,{status:'complete'});
    assert.equal(saved.text,changed?'new text':'before  after');
  }
});
