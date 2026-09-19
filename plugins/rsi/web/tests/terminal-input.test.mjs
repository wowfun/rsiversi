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
 pump.push(new Uint8Array([1]));pump.push(new Uint8Array([2]));release();await settled(pump);pump.push(new Uint8Array([3]));assert.equal(writes,1);assert(pump.stopped);assert.equal(pump.queue.length,0);assert.equal(failures.length,1);
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
