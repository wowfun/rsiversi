import 'fake-indexeddb/auto';
import {IDBDatabase} from 'fake-indexeddb';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {StartingDraft} from '../starting-draft.js';

test('recovery refuses malformed durable keys without modifying saved records',async()=>{
  const namespace=['key-validation','local'];
  const draft=await StartingDraft.load(namespace,'main');draft.edit('retained');await draft.flush();
  const db=await new Promise((resolve,reject)=>{
    const request=indexedDB.open('rsi.composer-starts');
    request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
  });
  const mutate=(key,remove=false)=>new Promise((resolve,reject)=>{
    const tx=db.transaction('drafts','readwrite');
    if(remove)tx.objectStore('drafts').delete(key);else tx.objectStore('drafts').put(draft.record,key);
    tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error);
  });
  try {
    for(const suffix of ['null]','42]','"bad/surface"]','"main","extra"]','"unterminated']) {
      const key=JSON.stringify(namespace).slice(0,-1)+','+suffix;
      await mutate(key);
      try {
        await assert.rejects(StartingDraft.list(namespace),/Invalid starting draft/);
        assert.equal((await StartingDraft.load(namespace,'main')).text,'retained');
      } finally {await mutate(key,true);}
    }
    await assert.rejects(StartingDraft.load(namespace,undefined),/Invalid starting draft surface/);
    await assert.rejects(StartingDraft.list(namespace,JSON.stringify(['another','local','main'])),/Invalid starting draft/);
    await assert.rejects(StartingDraft.remove({...draft.record,key:'malformed'}),/Invalid starting draft/);
  } finally {db.close();await clear(namespace);}
});

test('resolving saved starting input preserves typing during the storage read',async(t)=>{
  const draft=await StartingDraft.load(['starting-race','local'],'main');
  draft.edit('saved');await draft.flush();
  draft.failure=Error('conflict');draft.text='local before resolution';draft.dirty=true;
  const entered=Promise.withResolvers();let release,armed=true;
  const transaction=IDBDatabase.prototype.transaction;
  t.mock.method(IDBDatabase.prototype,'transaction',function(...args){
    const tx=transaction.apply(this,args);
    if(armed && tx.mode==='readonly'){
      armed=false;
      Object.defineProperty(tx,'oncomplete',{set(callback){tx.addEventListener('complete',event=>{release=()=>callback(event);entered.resolve()})}});
    }
    return tx;
  });
  const resolving=draft.resolve(true);await entered.promise;
  draft.edit('typed during resolution');release();await resolving;
  assert.equal(draft.text,'typed during resolution');assert.equal(draft.dirty,false);
  assert.equal((await StartingDraft.load(['starting-race','local'],'main')).text,'typed during resolution');
});

async function clear(namespace) {
  let page;
  do { page=await StartingDraft.list(namespace); for(const row of page.rows)await StartingDraft.remove(row); } while(page.next);
}

test('completed handoff clears both text and workspace of the captured starting draft',async()=>{
  const namespace=['completed-handoff','local'];
  const draft=await StartingDraft.load(namespace,'main');
  draft.edit('handed off','workspace');await draft.flush();
  await draft.clearCaptured(draft.serial);
  assert.deepEqual((await StartingDraft.list(namespace)).rows,[]);
  draft.edit('new input','new-workspace');await draft.flush();
  await draft.clearCaptured(draft.serial-1);
  assert.equal((await StartingDraft.load(namespace,'main')).text,'new input');
  await clear(namespace);
});
test('aligned saved-draft totals end without an empty recovery page',async()=>{
  for (const count of [32,33,64]) {
    const namespace=[`page-boundary-${count}`,'local'];
    for(let index=0;index<count;index++) {
      const draft=await StartingDraft.load(namespace,`pane-${index}`);
      draft.edit('retained');await draft.flush();
    }
    let after,seen=0,pages=0;
    do {
      const page=await StartingDraft.list(namespace,after);
      assert.ok(page.rows.length>0,'a continuation must contain a retained draft');
      seen+=page.rows.length;pages++;after=page.next;
    } while(after);
    assert.equal(seen,count);assert.equal(pages,Math.ceil(count/32));
    await clear(namespace);
  }
});
test('starting drafts bound origin retention, retain workspace choices and reject stale recreation',async()=>{
  await clear(['starting-race','local']);
  const namespace=['bounded-starts','local'];
  for(let i=0;i<128;i++) {
    const draft=await StartingDraft.load(namespace,`pane${i}`);
    draft.edit('',`workspace${i}`);await draft.flush();
  }
  const extra=await StartingDraft.load(namespace,'extra');extra.edit('kept locally');
  await assert.rejects(extra.flush(),/origin storage limit/);
  assert.equal(extra.text,'kept locally');
  const stale=await StartingDraft.load(namespace,'pane0');
  await StartingDraft.remove((await StartingDraft.list(namespace)).rows.find(row=>JSON.parse(row.key)[2]==='pane0'));
  stale.edit('must not resurrect');await assert.rejects(stale.flush(),/another window/);
  await clear(namespace);
  for(let i=0;i<4;i++) {
    const draft=await StartingDraft.load(namespace,`large${i}`);draft.edit('x'.repeat(1024*1024));await draft.flush();
  }
  const fifth=await StartingDraft.load(namespace,'fifth');fifth.edit('x');await assert.rejects(fifth.flush(),/origin storage limit/);
  await clear(namespace);
  const blank=await StartingDraft.load(namespace,'blank');blank.edit('text');await blank.flush();
  await blank.clearCaptured(blank.serial);assert.equal((await StartingDraft.list(namespace)).rows.length,0);
  blank.edit('again','workspace');await blank.flush();await blank.clearCaptured(blank.serial);
  assert.equal((await StartingDraft.list(namespace)).rows.length,0);
  await clear(namespace);
});

test('unrelated deletions do not conflict with blank editors; first creation remains atomic',async()=>{
  const namespace=['blank-intent','local'];
  const blank=await StartingDraft.load(namespace,'blank'),racing=await StartingDraft.load(namespace,'blank');
  const other=await StartingDraft.load(namespace,'other');other.edit('other saved input');await other.flush();
  await StartingDraft.remove((await StartingDraft.list(namespace)).rows[0]);
  blank.edit('new intent');await blank.flush();
  racing.edit('cannot overwrite first creator');await assert.rejects(racing.flush(),/another window/);
  assert.equal((await StartingDraft.load(namespace,'blank')).text,'new intent');
  await clear(namespace);
});

test('never-persisted absent readers create fresh intent after a same-key create/delete, while stored readers cannot resurrect',async()=>{
  const namespace=['fresh-after-delete','local'];
  const blank=await StartingDraft.load(namespace,'main');
  const stored=await StartingDraft.load(namespace,'main');stored.edit('old stored input');await stored.flush();
  await StartingDraft.remove((await StartingDraft.list(namespace)).rows[0]);
  stored.edit('stale stored input');await assert.rejects(stored.flush(),/another window/);
  blank.edit('fresh input');await blank.flush();
  assert.equal((await StartingDraft.load(namespace,'main')).text,'fresh input');
  await clear(namespace);
});
