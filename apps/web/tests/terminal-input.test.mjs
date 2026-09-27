import assert from 'node:assert/strict';
import {test} from 'node:test';
import {setImmediate} from 'node:timers/promises';
import {TerminalInput} from '../src/terminal-input.mjs';
async function settled(pump){while(pump.working)await setImmediate();}
test('document preserves exact UTF-8 bytes and serializes batches',async()=>{
 let release;const gate=new Promise(resolve=>release=resolve),calls=[];const pump=new TerminalInput(async bytes=>{calls.push([...bytes]);await gate},()=>assert.fail('unexpected failure'));
 pump.push(new TextEncoder().encode('界'));pump.push(new Uint8Array([13]));assert.equal(calls.length,1);release();await settled(pump);assert.deepEqual(calls,[[231,149,140],[13]]);
});
test('failed dispatch stops queued input without replay or receipt policy in JavaScript',async()=>{
 let release;const gate=new Promise(resolve=>release=resolve),failures=[];let writes=0;const pump=new TerminalInput(async()=>{writes++;await gate;throw Error('unconfirmed')},message=>failures.push(message));
 pump.push(new Uint8Array([1]));pump.push(new Uint8Array([2]));release();await settled(pump);pump.push(new Uint8Array([3]));assert.equal(writes,1);assert(pump.stopped);assert.equal(pump.queuedBytes,0);assert.equal(failures.length,1);
});
test('queue limit includes in-flight bytes and never forwards overflow',async()=>{
 let release;const gate=new Promise(resolve=>release=resolve),calls=[];const pump=new TerminalInput(async bytes=>{calls.push(bytes.length);await gate},()=>{});
 pump.push(new Uint8Array(65536));pump.push(new Uint8Array([1]));assert(pump.stopped);release();await settled(pump);assert.deepEqual(calls,[65536]);
});
test('unadmitted input stays queued through backpressure and is sent exactly once',async()=>{
 const delivered=[],failures=[];let attempts=0;
 const pump=new TerminalInput(async bytes=>{if(++attempts<=12)throw Object.assign(Error('busy'),{notAdmitted:true});delivered.push([...bytes])},message=>failures.push(message),async()=>{});
 pump.push(new Uint8Array([231,149,140]));pump.push(new Uint8Array([13]));await settled(pump);
 assert.deepEqual(delivered,[[231,149,140],[13]]);assert.deepEqual(failures,[]);assert(!pump.stopped);
});
test('fragmented queued input copies a linear number of bytes including dispatch',async()=>{
 const original=Uint8Array.prototype.set;let copied=0,release;
 const gate=new Promise(resolve=>release=resolve),delivered=[];
 const pump=new TerminalInput(async bytes=>{delivered.push([...bytes]);await gate},()=>assert.fail('unexpected failure'));
 Uint8Array.prototype.set=function(source,offset){copied+=source.length;return original.call(this,source,offset)};
 try {
  for(let i=0;i<65536;i++)pump.push(Uint8Array.of(i%256));
  release();await settled(pump);
  assert.deepEqual(delivered.flat(),Array.from({length:65536},(_,i)=>i%256));
  assert(copied<=3*65536,`copied ${copied} bytes for 65536 input bytes`);
 }finally{Uint8Array.prototype.set=original;release();}
});
test('ring wrap preserves bytes and copies caller input before it can change',async()=>{
 const delivered=[],pump=new TerminalInput(async bytes=>delivered.push(...bytes),()=>assert.fail('unexpected failure'));
 for(let round=0;round<5;round++){
  const bytes=new Uint8Array(20000).fill(round);pump.push(bytes);bytes.fill(99);await settled(pump);
 }
 assert.deepEqual(delivered,Array.from({length:100000},(_,i)=>Math.floor(i/20000)));
});
