import assert from 'node:assert/strict';
import {test} from 'node:test';
import {defaultLayout,validateLayout,presentationKey} from '../presentation-store.js';
import {planPresentationWrite} from '../device-store.js';
import {parseLayout,conversationWidth} from '../presentation-layout.js';
test('layout storage is closed, clamped and scoped to the authenticated principal',()=>{
  assert.throws(()=>parseLayout({...defaultLayout,navigationWidth:280}),/Invalid layout/);
  for(const value of [null,[],{...defaultLayout,token:'forbidden'},{...defaultLayout,navigationWidth:NaN},{...defaultLayout,navigation:'invalid'}]) assert.deepEqual(validateLayout(value),defaultLayout);
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('two',{kind:'local'}));
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('one',{kind:'device',device_id:'abc'}));
  assert.throws(()=>presentationKey('one',{kind:'unknown'}));
});
test('layout eviction is bounded and retains the recently touched identity',()=>{
  const records=Array.from({length:64},(_,i)=>({key:String(i),used:i,value:defaultLayout,revision:'1'}));
  const touched=planPresentationWrite('layouts',records,'0',defaultLayout,70);
  assert.equal(touched.evicted,undefined);
  records[0]=touched.replacement;
  assert.equal(planPresentationWrite('layouts',records,'new',defaultLayout,71).evicted,'1');
  const tied=records.map(record=>({...record,used:1})).reverse();
  assert.equal(planPresentationWrite('layouts',tied,'new',defaultLayout,71).evicted,'0');
  assert.throws(()=>planPresentationWrite('layouts',[...records,records[0]],'new',defaultLayout,71));
  assert.throws(()=>planPresentationWrite('layouts',[{...records[0],key:'x'.repeat(4096)}],'new',defaultLayout,71));
  assert.throws(()=>planPresentationWrite('layouts',[records[0],records[0]],'new',defaultLayout,71));
});

test('obsolete pixel layouts reset without touching current preferences',()=>{
  assert.deepEqual(validateLayout({navigationWidth:180,resourcesWidth:300,navigationClosed:true,resourcesClosed:false}),defaultLayout);
  assert.deepEqual(validateLayout({...defaultLayout,detail:'verbose'}),{...defaultLayout,detail:'verbose'});
});

test('conversation width retains the preferred value while its display fits the column',()=>{
  for(const value of [undefined,639,640.5,NaN,Infinity,Number.MAX_SAFE_INTEGER+1,'800'])
    assert.throws(()=>parseLayout({...defaultLayout,conversationWidth:value}),/Invalid layout/);
  assert.equal(parseLayout({...defaultLayout,conversationWidth:1200}).conversationWidth,1200);
  assert.equal(conversationWidth(null,1440),880);
  assert.equal(conversationWidth(null,1200),768);
  assert.equal(conversationWidth(null,800),680);
  assert.equal(conversationWidth(null,390),390);
  assert.equal(conversationWidth(1200,390),390);
  assert.equal(conversationWidth(1200,1440),1200);
  assert.equal(conversationWidth(1200,1000),824);
  assert.throws(()=>conversationWidth(null,Infinity));
  const {conversationWidth:_,...old}=defaultLayout;
  assert.throws(()=>parseLayout({...old,version:3}));
});


test('unsaved layout intents remain bounded and cannot be cleared by an older save', async()=>{
  const {LayoutEdits}=await import('../layout-edits.js');const edits=new LayoutEdits();
  const collapse={kind:'workspace',id:'one',expanded:false};
  edits.add(collapse);edits.add({kind:'reveal_workspace',id:'one'});
  assert.equal(edits.first()[1],collapse);
  edits.saved(edits.first()[0],collapse);
  edits.add({kind:'patch',patch:{conversationWidth:700}});
  const [key,old]=edits.first();
  edits.add({kind:'patch',patch:{conversationWidth:810,detail:'verbose'}});
  edits.saved(key,old);assert.equal(edits.size,2);
  for(let i=0;i<100;i++)edits.add({kind:'workspace',id:String(i),expanded:false});
  assert.equal(edits.size,18);
  edits.add({kind:'reset'});assert.equal(edits.size,1);
  edits.add({kind:'patch',patch:{resourcesClosed:false}});assert.equal(edits.size,2);
  assert.equal(edits.first()[1].kind,'reset');
});
