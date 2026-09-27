import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readTerminalPage} from '../src/terminal-read.mjs';
test('lost response retries the same Rust-owned output ACK without skipping a page', async()=>{
  const seen=[], waits=[]; const page={ack:'18',value:{text:'exactly once'}};
  const result=await readTerminalPage(async ack=>{seen.push(ack);if(seen.length<3)throw Error('transport interrupted');return page}, '17', ()=>true, async ms=>{waits.push(ms)});
  assert.equal(result,page);assert.deepEqual(seen,['17','17','17']);assert.deepEqual(waits,[100,250]);
});
test('permanent failure has a finite retry bound and detach cancels retry', async()=>{
  let calls=0;await assert.rejects(readTerminalPage(async()=>{calls++;throw Error('unavailable')},undefined,()=>true,async()=>{}),/unavailable/);assert.equal(calls,4);
  let active=true;calls=0;assert.equal(await readTerminalPage(async()=>{calls++;throw Error('transport')},'2',()=>active,async()=>{active=false}),undefined);assert.equal(calls,1);
});
test('admission backpressure outlasts the ambiguous retry bound and preserves ACK',async()=>{
  let calls=0;const waits=[];
  const result=await readTerminalPage(async ack=>{assert.equal(ack,'42');if(++calls<=12)throw Object.assign(Error('busy'),{notAdmitted:true});return 'page'},'42',()=>true,async ms=>waits.push(ms));
  assert.equal(result,'page');assert.equal(calls,13);assert(waits.every(ms=>ms<=500));
});
