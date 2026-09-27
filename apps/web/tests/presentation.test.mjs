import assert from 'node:assert/strict';
import {test} from 'node:test';
import {defaultLayout,validateLayout,presentationKey,boundedRecords} from '../presentation-store.js';
test('layout storage is closed, clamped and scoped to the authenticated principal',()=>{
  assert.deepEqual(validateLayout({...defaultLayout,navigationWidth:900,resourcesWidth:1}),{...defaultLayout,navigationWidth:420,resourcesWidth:210});
  for(const value of [null,[],{...defaultLayout,token:'forbidden'},{...defaultLayout,navigationWidth:NaN},{...defaultLayout,navigation:'invalid'}]) assert.deepEqual(validateLayout(value),defaultLayout);
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('two',{kind:'local'}));
  assert.notEqual(presentationKey('one',{kind:'local'}),presentationKey('one',{kind:'device',device_id:'abc'}));
  assert.throws(()=>presentationKey('one',{kind:'unknown'}));
});
test('layout eviction is bounded and retains the recently touched identity',()=>{
  const records=Array.from({length:64},(_,i)=>({key:String(i),used:i,layout:defaultLayout}));
  const touched=boundedRecords(records,'0',defaultLayout,70);
  const added=boundedRecords(touched,'new',defaultLayout,71);
  assert.equal(added.length,64);assert(added.some(record=>record.key==='0'));assert(!added.some(record=>record.key==='1'));
  assert.throws(()=>boundedRecords([...records,records[0]],'new',defaultLayout,71));
  assert.throws(()=>boundedRecords([{...records[0],key:'x'.repeat(4096)}],'new',defaultLayout,71));
  assert.throws(()=>boundedRecords([records[0],records[0]],'new',defaultLayout,71));
});

test('legacy layout migration resets resource visibility without touching current preferences',()=>{
  assert.deepEqual(validateLayout({navigationWidth:180,resourcesWidth:300,navigationClosed:true,resourcesClosed:false}),{...defaultLayout,navigationWidth:264,resourcesWidth:300,navigation:'rail'});
  assert.deepEqual(validateLayout({...defaultLayout,navigation:'hidden',detail:'verbose'}),{...defaultLayout,navigation:'hidden',detail:'verbose'});
});
