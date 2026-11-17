import 'fake-indexeddb/auto';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {StartingDraft} from '../starting-draft.js';
import {DraftStore} from '../drafts.js';

test('all-drafts reset erases both persisted formats and retires old editors',async()=>{
  globalThis.window=new EventTarget();let retired=0;
  window.addEventListener('rsi:draft-storage-retired',()=>retired++);
  const namespace=['a'.repeat(32),'local'];
  const start=await StartingDraft.load(namespace,'main');start.edit('private starting text');await start.flush();
  const store=await DraftStore.open(namespace[0],{kind:'local'});
  const blank=await store.ensure('main','session','b'.repeat(64));
  await store.edit(blank,'private session text',[]);
  // A fresh XDG namespace creates a new endpoint; old device drafts still count.
  const fresh=await DraftStore.open('c'.repeat(32),{kind:'local'});
  assert.equal(await fresh.get('main','session'),undefined);
  await fresh.edit(await fresh.ensure('main','new-session','b'.repeat(64)),'new endpoint text',[]);
  const freshStart=await StartingDraft.load(fresh.namespace,'main');
  freshStart.edit('new starting text');await freshStart.flush();
  assert.equal((await store.get('main','session')).text,'private session text');
  await DraftStore.discardAllDrafts();
  assert.equal(retired,1);
  assert.deepEqual(await indexedDB.databases(),[]);
  assert.throws(()=>start.edit('late save'),/reset or upgraded/);
  await assert.rejects(store.get('main','session'),/reset or upgraded/);
  await assert.rejects(fresh.get('main','new-session'),/reset or upgraded/);
  await assert.rejects(StartingDraft.load(namespace,'main'),/reset or upgraded/);
  delete globalThis.window;
});
