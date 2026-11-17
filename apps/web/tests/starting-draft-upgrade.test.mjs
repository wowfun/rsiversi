import 'fake-indexeddb/auto';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {StartingDraft} from '../starting-draft.js';

test('schema2 upgrade preserves stored identity/text and removes obsolete deletion metadata',async()=>{
  const key=JSON.stringify(['endpoint','local','main']);
  const record={revision:7,incarnation:crypto.randomUUID(),text:'saved input',workspace:'workspace'};
  await new Promise((resolve,reject)=>{
    const request=indexedDB.open('rsi.composer-starts',2);
    request.onupgradeneeded=()=>{const db=request.result;db.createObjectStore('drafts').put(record,key);db.createObjectStore('usage').put({counts:[1,11],epoch:9},'aggregate');};
    request.onsuccess=()=>{request.result.close();resolve();};request.onerror=()=>reject(request.error);
  });
  const draft=await StartingDraft.load(['endpoint','local'],'main');assert.deepEqual(draft.record,record);
  const request=indexedDB.open('rsi.composer-starts');
  const db=await new Promise((resolve,reject)=>{request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
  assert.equal(db.version,3);
  const usage=db.transaction('usage').objectStore('usage').get('aggregate');
  assert.deepEqual(await new Promise(resolve=>usage.onsuccess=()=>resolve(usage.result)),{counts:[1,11]});db.close();
});
