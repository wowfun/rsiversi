import assert from 'node:assert/strict';
import {test} from 'node:test';
import {DraftStore,validateEditor} from '../drafts.js';
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

test('explicit discard removes only the composer database and reports blocked deletion', async () => {
  const original = globalThis.indexedDB;
  let request, name;
  globalThis.indexedDB = {deleteDatabase(value){ name=value; request={}; return request; }};
  try {
    const discard = DraftStore.discardAllSaved();
    assert.equal(name, 'rsi.composer');
    request.onsuccess(); await discard;
    const blocked = DraftStore.discardAllSaved();
    request.onblocked();
    await assert.rejects(blocked, /Close other RSIversi tabs/);
  } finally { globalThis.indexedDB = original; }
});

test('command controls retain editor content through unknown-result recovery and completion', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  let saved = store.blank('main','session','b'.repeat(64));
  saved.text = 'Unsubmitted work';
  store.mutate = async (_key, change) => saved = change(structuredClone(saved));
  saved = await store.freeze(saved, {kind:'command', id:'command-1',opaque:'frozen',text_bytes:0,images:0,references:0}, true);
  saved = await store.begin(saved);
  saved = await store.settle(saved, {status:'unknown'});
  assert.equal(saved.pending.opaque, 'frozen');
  assert.equal(saved.pending.preserve_editor, true);
  saved = await store.settle(saved, {status:'complete',receipt:'confirmed'});
  assert.equal(saved.text, 'Unsubmitted work');
  assert.equal(saved.pending, null);
  assert.throws(() => store.freeze(saved,{kind:'message'},true), /Only a command/);
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
