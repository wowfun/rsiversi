import test from 'node:test';
import assert from 'node:assert/strict';
import {createSettingsDraft} from '../src/runtime/settings-draft.js';
const view=(ticket='1',version='1',text='{"limit":2}')=>({ticket,version,namespace:'fixture.numbers',text,description:{writable:true,defaults:{limit:2},metadata:{applies:'live',description:'',sensitive_fields:[],schema:{type:'object',properties:{limit:{type:'number'}},required:['limit']}}}});
test('numeric form edits keep exact JSON lexemes through mode switches and failed save',async()=>{
 const calls=[],draft=createSettingsDraft(view(),async value=>{calls.push(value);throw Error('offline')});
 draft.fields.edit(draft.fields.fields.parts[0].field,'1.0000000000000001');
 await draft.save();assert.equal(calls[0].text,'{"limit":1.0000000000000001}');
 draft.fields.toggle();assert.match(draft.fields.text,/1\.0000000000000001/);assert.match(draft.error,/offline/);
 draft.fields.toggle();assert.equal(draft.fields.jsonMode,true);assert.match(draft.fields.reason,/exact numbers/);
});
test('dirty settings keep their version and text until an explicit reload',async()=>{
 const draft=createSettingsDraft(view(),async()=>{});draft.fields.edit(draft.fields.fields.parts[0].field,'3');
 draft.update(view('2','2','{"limit":5}'));assert.equal(draft.fields.json(),'{"limit":3}');assert.equal(draft.canSave,false);assert.match(draft.error,/Settings changed/);
 await draft.reload();assert.equal(draft.awaiting,true);draft.update(view('3','2','{"limit":5}'));assert.equal(draft.fields.json(),'{"limit":5}');assert.equal(draft.canSave,true);
});
test('saving coalesces calls and waits for a fresh presentation',async()=>{
 let resolve,calls=0;const draft=createSettingsDraft(view(),()=>{calls++;return new Promise(done=>resolve=done)});
 const pending=draft.save();await draft.save();assert.equal(calls,1);assert.equal(draft.disabled,true);resolve();await pending;
 assert.equal(draft.canSave,false);draft.update(view('2'));assert.equal(draft.canSave,true);
});
test('presentation mode changes alone do not freeze a clean editor; retirement fences updates',()=>{
 const draft=createSettingsDraft(view(),async()=>{});draft.fields.toggle();draft.update(view('2','2','{"limit":8}'));assert.equal(draft.fields.json(),'{"limit":8}');
 draft.retire();draft.update(view('3','3','{"limit":9}'));assert.equal(draft.fields.json(),'{"limit":8}');
});
