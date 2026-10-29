import assert from 'node:assert/strict';
import {test} from 'node:test';
import {defaultLayout,validateLayout,presentationKey} from '../presentation-store.js';
import {boundedRecords} from '../device-store.js';
import {parseLayout,conversationWidth} from '../presentation-layout.js';
test('layout storage is closed, clamped and scoped to the authenticated principal',()=>{
  assert.deepEqual(validateLayout({...defaultLayout,navigationWidth:900,resourcesWidth:1}),{...defaultLayout,navigationWidth:420,resourcesWidth:.7});
  for(const value of [null,[],{...defaultLayout,token:'forbidden'},{...defaultLayout,navigationWidth:NaN},{...defaultLayout,navigation:'invalid'}]) assert.deepEqual(validateLayout(value),defaultLayout);
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('two',{kind:'local'}));
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('one',{kind:'device',device_id:'abc'}));
  assert.throws(()=>presentationKey('one',{kind:'unknown'}));
});
test('layout eviction is bounded and retains the recently touched identity',()=>{
  const records=Array.from({length:64},(_,i)=>({key:String(i),used:i,value:defaultLayout,revision:'1'}));
  const touched=boundedRecords('layouts',records,'0',defaultLayout,70).records;
  const added=boundedRecords('layouts',touched,'new',defaultLayout,71).records;
  assert.equal(added.length,64);assert(added.some(record=>record.key==='0'));assert(!added.some(record=>record.key==='1'));
  assert.throws(()=>boundedRecords('layouts',[...records,records[0]],'new',defaultLayout,71));
  assert.throws(()=>boundedRecords('layouts',[{...records[0],key:'x'.repeat(4096)}],'new',defaultLayout,71));
  assert.throws(()=>boundedRecords('layouts',[records[0],records[0]],'new',defaultLayout,71));
});

test('obsolete pixel layouts reset without touching current preferences',()=>{
  assert.deepEqual(validateLayout({navigationWidth:180,resourcesWidth:300,navigationClosed:true,resourcesClosed:false}),defaultLayout);
  assert.deepEqual(validateLayout({...defaultLayout,navigation:'hidden',detail:'verbose'}),{...defaultLayout,navigation:'hidden',detail:'verbose'});
});

test('conversation width retains the preferred value while its display fits the column',()=>{
  for(const value of [undefined,639,640.5,NaN,Infinity,Number.MAX_SAFE_INTEGER+1,'800'])
    assert.throws(()=>parseLayout({...defaultLayout,conversationWidth:value}),/Invalid layout/);
  assert.equal(parseLayout({...defaultLayout,conversationWidth:1200}).conversationWidth,1200);
  assert.equal(conversationWidth(null,1440),920);
  assert.equal(conversationWidth(null,1200),768);
  assert.equal(conversationWidth(null,800),680);
  assert.equal(conversationWidth(null,390),390);
  assert.equal(conversationWidth(1200,390),390);
  assert.equal(conversationWidth(1200,1440),1200);
  assert.throws(()=>conversationWidth(null,Infinity));
  const {conversationWidth:_,...old}=defaultLayout;
  assert.throws(()=>parseLayout({...old,version:3}));
});


test('unsaved layout intents remain bounded and cannot be cleared by an older save', async()=>{
  const {LayoutEdits}=await import('../layout-edits.js');const edits=new LayoutEdits();
  edits.add({kind:'patch',patch:{navigationWidth:300}});
  const [key,old]=edits.first();
  edits.add({kind:'patch',patch:{navigationWidth:410,detail:'verbose'}});
  edits.saved(key,old);assert.equal(edits.size,2);
  for(let i=0;i<100;i++)edits.add({kind:'workspace',id:String(i),expanded:false});
  assert.equal(edits.size,18);
  edits.add({kind:'reset'});assert.equal(edits.size,1);
  edits.add({kind:'patch',patch:{navigation:'rail'}});assert.equal(edits.size,2);
  assert.equal(edits.first()[1].kind,'reset');
});
