import 'fake-indexeddb/auto';
import {IDBDatabase} from 'fake-indexeddb';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {StartingDraft} from '../starting-draft.js';

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
