import 'fake-indexeddb/auto';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {DraftStore} from '../drafts.js';
import {StartingDraft} from '../starting-draft.js';

async function seed(name,version,write) {
  await new Promise((resolve,reject)=>{
    const request=indexedDB.open(name,version);
    request.onupgradeneeded=()=>write(request.result);
    request.onsuccess=()=>{request.result.close();resolve();};request.onerror=()=>reject(request.error);
  });
}
test('known draft upgrades preserve frozen requests and allow reducing legacy starting quota debt',async()=>{
  const namespace=['a'.repeat(32),'local'];
  const previous=new DraftStore(undefined,...namespace).blank('main','session','b'.repeat(64));
  previous.version=7;previous.text='saved text';previous.editRevision=4;
  previous.pending={kind:'command',id:'same-request',opaque:'exact request',text_bytes:10,images:0,captures:[],phase:'unknown',editRevision:3,preserve_editor:true};
  await seed('rsi.composer',7,db=>{
    const drafts=db.createObjectStore('drafts',{keyPath:'key'});drafts.createIndex('scope','scope');drafts.put(previous);
    db.createObjectStore('usage').put([1,10,10,13],'aggregate');
  });
  const store=await DraftStore.open(namespace[0],{kind:'local'}),upgraded=await store.get('main','session');
  assert.deepEqual(upgraded,{...previous,version:8});await store.verify();
  await seed('rsi.composer-starts',1,db=>{
    const drafts=db.createObjectStore('drafts');
    for(let index=0;index<129;index++)drafts.put({revision:1,text:index===0?'retained':'',workspace:'workspace'},JSON.stringify([...namespace,`pane${index}`]));
    drafts.put({revision:3,text:'',workspace:null},JSON.stringify([...namespace,'blank']));
  });
  const first=await StartingDraft.load(namespace,'pane0');assert.equal(first.text,'retained');
  const extra=await StartingDraft.load(namespace,'extra');extra.edit('new input');await assert.rejects(extra.flush(),/origin storage limit/);
  first.edit('less');await first.flush();assert.equal(first.text,'less');
  let after,rows=[];
  do {const page=await StartingDraft.list(namespace,after);assert(page.rows.length<=32);rows.push(...page.rows);after=page.next;}while(after);
  assert.equal(rows.length,129);assert(rows.every(row=>row.incarnation.length===36));
  await StartingDraft.remove(rows[0]);await StartingDraft.remove(rows[1]);
  await extra.resolve(false);assert.equal((await StartingDraft.load(namespace,'extra')).text,'new input');
});
