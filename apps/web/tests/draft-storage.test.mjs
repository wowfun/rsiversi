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
