import 'fake-indexeddb/auto';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {StartingDraft} from '../starting-draft.js';

test('invalid schema2 counters abort the upgrade without changing saved input or metadata',async()=>{
  const key=JSON.stringify(['endpoint','local','main']);
  const record={revision:7,incarnation:crypto.randomUUID(),text:'saved input',workspace:'workspace'};
  const aggregate={counts:[-1,11],epoch:9};
  await new Promise((resolve,reject)=>{
    const request=indexedDB.open('rsi.composer-starts',2);
    request.onupgradeneeded=()=>{const db=request.result;db.createObjectStore('drafts').put(record,key);db.createObjectStore('usage').put(aggregate,'aggregate');};
    request.onsuccess=()=>{request.result.close();resolve();};request.onerror=()=>reject(request.error);
  });
  await assert.rejects(StartingDraft.load(['endpoint','local'],'main'),/Invalid starting draft counters.*Saved drafts remain/);
  const request=indexedDB.open('rsi.composer-starts');
  const db=await new Promise((resolve,reject)=>{request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
  assert.equal(db.version,2);
  const tx=db.transaction(['drafts','usage']);
  const read=request=>new Promise(resolve=>request.onsuccess=()=>resolve(request.result));
  const [saved,usage]=await Promise.all([read(tx.objectStore('drafts').get(key)),read(tx.objectStore('usage').get('aggregate'))]);
  assert.deepEqual(saved,record);assert.deepEqual(usage,aggregate);db.close();
});
