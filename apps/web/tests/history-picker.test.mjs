import test from 'node:test';
import assert from 'node:assert/strict';
import {createHistoryPicker} from '../src/runtime/history-picker.js';
const scope={workspace:'workspace',conversation:{id:'source',kind:'native'}};
const hit={preview:'text',original:{record:{kind:'assistant',sequence:'1'},end:8}};
const reference={snapshot:{capture_id:'pending'},preview:'text'};
const progress={visible_sources:1,pending_sources:0,discovery_complete:true};
function setup(override={}) {
 const released=[],added=[],requests=[];
 const picker=createHistoryPicker({context:{workspace:'workspace',session:'target'},workspaces:[],sources:[],active:()=>true,changed(){},closed(){},
  release:async value=>released.push(value),add:value=>added.push(value),
  request:async request=>{requests.push(request);return request.operation==='query'?{progress,matches:[{scope,hit,reference_allowed:true}]}:request.operation==='read'?{offset:2,next_offset:8,text:'中abc',has_more:false}:{reference};},...override});
 return {picker,released,added,requests};
}
test('replacing a frozen history preview releases it once without waiting for close',async()=>{
 const {picker,released}=setup();await picker.read(scope,hit);await picker.freeze(0,1);
 assert.deepEqual(released,[]);await picker.search();assert.deepEqual(released,[reference]);picker.close();assert.equal(released.length,1);
});
test('selected original offsets use UTF-8 bytes; adding transfers ownership',async()=>{
 const {picker,released,added,requests}=setup();await picker.read(scope,hit);await picker.freeze(1,3);
 assert.deepEqual(requests.at(-1),{operation:'freeze',scope,hit,target:'target',start:5,end:7});
 picker.add();picker.close();assert.deepEqual(added,[reference]);assert.deepEqual(released,[]);
});
test('a freeze completing after retirement is released and cannot reopen the dialog',async()=>{
 let resolve;const {picker,released}=setup({request:request=>request.operation==='freeze'?new Promise(done=>resolve=done):Promise.resolve({offset:0,text:'text'})});
 await picker.read(scope,hit);const pending=picker.freeze(0,2);picker.close();resolve({reference});await pending;
 assert.deepEqual(released,[reference]);assert.equal(picker.body,undefined);
});
test('protected originals and empty selections cannot freeze',async()=>{
 const {picker,requests}=setup();await picker.read(scope,hit,0,false);await picker.freeze(0,1);assert.equal(requests.length,1);
 await picker.read(scope,hit);await picker.freeze(1,1);assert.equal(requests.length,2);assert.match(picker.status,/nonempty/);
});
