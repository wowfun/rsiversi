import test from 'node:test';
import assert from 'node:assert/strict';
import {createReferencePicker} from '../src/runtime/reference-picker.js';
const reference={preview:'Frozen',metadata:{text_bytes:12},snapshot:{sha256:'original'}};
function picker(ports={}) {
  return createReferencePicker({active:()=>true,describe:()=>({origin:'Source'}),canAdd:()=>true,changed(){},closed(){},add(){},release:async()=>{},request:async()=>reference,...ports});
}

test('capture returned after close is released exactly once without replacing the retired preview',async()=>{
  let resolve,closed=0;const released=[];
  const value=picker({request:()=>new Promise(done=>resolve=done),release:async item=>released.push(item),closed(){closed++;}});
  const pending=value.capture('source');await Promise.resolve();value.close();value.close();resolve(reference);await pending;
  assert.equal(closed,1);assert.deepEqual(released,[reference]);assert.equal(value.preview,undefined);
});

test('adding transfers capture ownership to the draft; a later close cannot release it',async()=>{
  const added=[],released=[];const value=picker({add:item=>added.push(item),release:async item=>released.push(item)});
  await value.capture(' source ');value.add();value.close();await Promise.resolve();
  assert.deepEqual(added,[reference]);assert.deepEqual(released,[]);
});

test('closing during prior capture cleanup prevents a new capture request',async()=>{
  let release,requests=0,releases=0;const value=picker({request:async()=>{requests++;return reference},release:()=>{releases++;return new Promise(done=>release=done)}});
  await value.capture('first');const second=value.capture('second');value.close();release();await second;
  assert.equal(requests,1);assert.equal(value.preview,undefined);
  assert.equal(releases,1);
});

test('out-of-order frozen preview pages retain only the most recent request',async()=>{
  const replies=[];const value=picker({existing:reference,request:()=>new Promise(done=>replies.push(done))});
  const first=value.page(8192),second=value.page(16384);
  replies[1]({text:'New page',offset:16384,next_offset:18000,has_more:false});await second;
  replies[0]({text:'Old page',offset:8192,next_offset:16384,has_more:true});await first;
  assert.equal(value.preview.text,'New page');assert.equal(value.preview.previous,8192);assert.equal(value.preview.next,undefined);
});

test('cleanup refusal retains a diagnostic and does not issue another capture',async()=>{
  let requests=0,releases=0;const value=picker({request:async()=>{requests++;return reference},release:async()=>{releases++;throw new Error('Reference cleanup failed: offline')}});
  await value.capture('first');await value.capture('second');await value.capture('third');
  assert.equal(requests,1);assert.match(value.status,/Reference cleanup failed: offline/);assert.equal(value.capturing,false);
  assert.equal(releases,2);
});

test('successful cleanup retry releases the old identity before admitting another capture',async()=>{
  let requests=0,online=false;const released=[];
  const value=picker({request:async()=>({...reference,id:++requests}),release:async item=>{released.push(item.id);if(!online)throw Error('Reference cleanup failed: offline')}});
  await value.capture('first');await value.capture('second');online=true;await value.capture('third');
  assert.deepEqual(released,[1,1]);assert.equal(requests,2);assert.equal(value.preview.reference.id,2);
});
