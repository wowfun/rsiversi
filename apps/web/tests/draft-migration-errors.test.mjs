import 'fake-indexeddb/auto';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {DraftStore} from '../drafts.js';
import {StartingDraft} from '../starting-draft.js';

async function database(name,version,upgrade) {
  return new Promise((resolve,reject)=>{
    const request=indexedDB.open(name,version);
    request.onupgradeneeded=()=>upgrade?.(request.result);
    request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
  });
}
async function inspect(name) {
  const db=await database(name);
  const rows=await new Promise((resolve,reject)=>{
    const request=db.transaction('drafts').objectStore('drafts').getAll();
    request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
  });
  const result={version:db.version,stores:[...db.objectStoreNames],rows};db.close();return result;
}
test('asynchronous composer upgrade validation preserves the reason and rolls back prior cursor writes',async()=>{
  const store=new DraftStore(undefined,'a'.repeat(32),'local');
  const first={...store.blank('a','session','b'.repeat(64)),version:7};
  const invalid={...store.blank('z','session','b'.repeat(64)),version:7,text:123};
  first.scope=first.key.slice(0,3);invalid.scope=invalid.key.slice(0,3);
  const db=await database('rsi.composer',7,db=>{
    const records=db.createObjectStore('drafts',{keyPath:'key'});records.createIndex('scope','scope');records.put(first);records.put(invalid);db.createObjectStore('usage');
  });db.close();
  await assert.rejects(DraftStore.open('a'.repeat(32),{kind:'local'}),/Draft text must be a string.*saved drafts remain on this device/i);
  assert.deepEqual(await inspect('rsi.composer'),{version:7,stores:['drafts','usage'],rows:[first,invalid]});
});
test('asynchronous starting upgrade validation retains its reason and leaves schema1 intact',async()=>{
  const first={revision:1,text:'keep exactly',workspace:null},invalid={revision:-1,text:'also retained',workspace:null};
  const db=await database('rsi.composer-starts',1,db=>{
    const records=db.createObjectStore('drafts');records.put(first,JSON.stringify(['endpoint','local','a']));records.put(invalid,JSON.stringify(['endpoint','local','z']));
  });db.close();
  await assert.rejects(StartingDraft.load(['endpoint','local'],'a'),/Invalid starting draft.*saved drafts remain on this device/i);
  assert.deepEqual(await inspect('rsi.composer-starts'),{version:1,stores:['drafts'],rows:[first,invalid]});
});
