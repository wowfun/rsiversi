import test from 'node:test';
import assert from 'node:assert/strict';
import {createHighlightQueue} from '../src/runtime/highlight-queue.js';

function fixture(highlight=()=>[], now) {
  const frames=new Set();
  const queue=createHighlightQueue({highlight,now,schedule:run=>{frames.add(run);return ()=>frames.delete(run)}});
  return {queue,frames,tick(){const run=frames.values().next().value;assert(run);frames.delete(run);run()}};
}
test('all code blocks share one deferred attempt per animation frame',()=>{
  const calls=[],done=[];const f=fixture((text,language,options)=>{calls.push({text,language,options});return []});
  for(let i=0;i<60;i++)f.queue.enqueue(String(i),'js',()=>done.push(i));
  assert.equal(calls.length,0);assert.equal(f.frames.size,1);
  for(let i=0;i<60;i++){f.tick();assert.equal(calls.length,i+1)}
  assert.equal(f.frames.size,0);assert.equal(done.length,60);
  assert(calls.every(call=>call.options.budgetMs===16));
});
test('only time-exhausted work requeues without starving other blocks',()=>{
  const attempted=[],done=[];let count=0;
  const f=fixture((text,_language,options)=>{attempted.push(text);if(text==='bad')throw Error('grammar');if(text==='slow'&&count++<4){options.onBudgetExceeded();return null}return text==='unsupported'?null:[]});
  for(const text of ['slow','bad','unsupported'])f.queue.enqueue(text,'js',tokens=>done.push([text,tokens]));
  for(let i=0;i<7;i++)f.tick();
  assert.deepEqual(attempted,['slow','bad','unsupported','slow','slow','slow','slow']);
  assert.deepEqual(done,[['bad',null],['unsupported',null],['slow',[]]]);assert.equal(f.frames.size,0);
});
test('retries are bounded by accumulated work and attempt count',()=>{
  for(const duration of [0,100]) {
    let now=0,calls=0,done=false;
    const f=fixture((_text,_language,options)=>{calls++;now+=duration;options.onBudgetExceeded();return null},()=>now);
    f.queue.enqueue('slow','js',tokens=>{assert.equal(tokens,null);done=true});
    for(let i=0;i<(duration?5:32);i++)f.tick();
    assert(done);assert.equal(calls,duration?5:32);assert.equal(f.frames.size,0);
  }
});
test('cancellation releases queued source and suppresses late results',()=>{
  let cancel;const f=fixture(()=>{cancel();return []});
  cancel=f.queue.enqueue('cancel while processing','js',assert.fail);f.tick();assert.equal(f.frames.size,0);
  const stop=f.queue.enqueue('retired','js',assert.fail);stop();assert.equal(f.frames.size,0);
});
test('job and source capacity are bounded and reusable after cancellation',()=>{
  for(const [text,limit] of [['x',128],['x'.repeat(256*1024),4]]) {
    const f=fixture(),done=[],cancel=[];
    for(let i=0;i<limit;i++)cancel.push(f.queue.enqueue(text,'js',()=>done.push(i)));
    f.queue.enqueue(text,'js',assert.fail);cancel[0]();
    f.queue.enqueue(text,'js',()=>done.push('replacement'));
    for(let i=0;i<limit;i++)f.tick();
    assert.equal(done.length,limit);assert.equal(done.at(-1),'replacement');assert.equal(f.frames.size,0);
  }
});
