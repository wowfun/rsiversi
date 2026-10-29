import test from 'node:test';
import assert from 'node:assert/strict';
import {createFilePicker} from '../src/runtime/file-picker.js';
const page={kind:'file',path:'746573742e747874',revision:'read-1',locator:'@test.txt',offset:'0',next_offset:'4',total:'4',more:false,text:'Text'};
const picker=ports=>createFilePicker({active:()=>true,request:async()=>page,changed(){},closed(){},insert(){},...ports});

test('closing an in-flight file read sends cancellation and cannot publish its late page',async()=>{
  let resolve,changes=0;const requests=[];
  const value=picker({changed(){changes++;},request:operation=>{requests.push(operation);return operation?new Promise(done=>resolve=done):Promise.resolve(null)}});
  const opening=value.open('','directory');value.close();const before=changes;resolve(page);await opening;
  assert.deepEqual(requests,[{kind:'open',path:'',file_kind:'directory'},null]);
  assert.equal(changes,before);assert.equal(value.page,undefined);
});

test('a file insertion uses the owner locator and releases only its admitted revision',async()=>{
  const inserted=[],requests=[];const value=picker({insert:text=>inserted.push(text),request:async operation=>{requests.push(operation);return page}});
  await value.openText('test.txt','file');value.insert();value.close();
  assert.deepEqual(inserted,['@test.txt']);assert.deepEqual(requests,[{kind:'open',path:page.path,file_kind:'file'},{kind:'release',revision:'read-1'}]);
});

test('changed draft or failed insertion keeps the picker open and reports the refusal',async()=>{
  let closed=0;const value=picker({closed(){closed++;},insert(){throw new Error('Draft changed')}});
  await value.open('','file');value.insert();assert.equal(closed,0);assert.match(value.status,/Draft changed/);
});

test('UTF-8 path bounds precede the read and undecodable paths retain exact bytes',async()=>{
  const requests=[];const value=picker({request:async operation=>{requests.push(operation);return {...page,path:'ff2f61'}}});
  await value.openText('😀'.repeat(1025),'file');assert.equal(requests.length,0);assert.match(value.status,/4 KiB/);
  await value.openText('.','directory');assert.equal(requests[0].path,'');assert.equal(value.page.label,'path_hex:ff2f61');assert.equal(value.page.parent,'ff');
});
