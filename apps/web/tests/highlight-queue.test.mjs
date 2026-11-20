import test from 'node:test';
import assert from 'node:assert/strict';
import {createHighlightQueue} from '../src/runtime/highlight-queue.js';
function fixture(step=()=>({status:'complete',tokens:[]}), now) {
  const frames=new Set(),created=[];
  const queue=createHighlightQueue({now,createSession:(text,language)=>{
    created.push(text);
    return {progress:0,retainedTokens:0,step(budget){return step.call(this,text,language,budget)}};
  },schedule:run=>{frames.add(run);return ()=>frames.delete(run)}});
  return {queue,frames,created,tick(){const run=frames.values().next().value;assert(run);frames.delete(run);run()}};
}
test('slices yield without recreating sessions',()=>{
  const calls=[],done=[];
  const f=fixture(function(text,_language,budget){assert.equal(budget,16);calls.push(text);this.progress++;return this.progress<3?{status:'pending'}:{status:'complete',tokens:[text]}});
  for(const text of ['first','second'])f.queue.enqueue(text,'js',tokens=>done.push(tokens));
  assert.equal(calls.length,0);assert.equal(f.frames.size,1);for(let i=0;i<6;i++)f.tick();
  assert.deepEqual(calls,['first','second','first','second','first','second']);
  assert.deepEqual(f.created,['first','second']);assert.deepEqual(done,[['first'],['second']]);assert.equal(f.frames.size,0);
});
test('no progress, grammar errors and accumulated work terminate',()=>{
  for(const kind of ['stuck','error','spent']){
    let time=0,done=false;
    const f=fixture(function(){if(kind==='error')throw Error('grammar');if(kind==='spent'){this.progress++;time+=100;}return {status:'pending'}},()=>time);
    f.queue.enqueue('source','js',tokens=>{assert.equal(tokens,null);done=true});
    for(let i=0;i<(kind==='spent'?5:1);i++)f.tick();assert(done);assert.equal(f.frames.size,0);
  }
});
test('cancellation suppresses completion and releases accounting',()=>{
  let cancel;const f=fixture(function(){this.retainedTokens=32768;cancel();return {status:'complete',tokens:[]}});
  cancel=f.queue.enqueue('source','js',assert.fail);f.tick();assert.equal(f.frames.size,0);
  const stop=f.queue.enqueue('retired','js',assert.fail);stop();assert.equal(f.frames.size,0);
});
test('retained tokens are bounded across unfinished blocks',()=>{
  const done=[];const f=fixture(function(text){this.progress++;this.retainedTokens=20000;return this.progress===1?{status:'pending'}:{status:'complete',tokens:[text]}});
  f.queue.enqueue('first','js',tokens=>done.push(tokens));f.queue.enqueue('second','js',tokens=>done.push(tokens));
  f.tick();f.tick();f.tick();assert.deepEqual(done,[null,['first']]);assert.equal(f.frames.size,0);
});
test('job and source capacity are reusable after cancellation',()=>{
  for(const [text,limit] of [['x',128],['x'.repeat(256*1024),4]]){
    const f=fixture(),done=[],cancel=[];
    for(let i=0;i<limit;i++)cancel.push(f.queue.enqueue(text,'js',()=>done.push(i)));
    f.queue.enqueue(text,'js',assert.fail);cancel[0]();f.queue.enqueue(text,'js',()=>done.push('replacement'));
    for(let i=0;i<limit;i++)f.tick();assert.equal(done.length,limit);assert.equal(done.at(-1),'replacement');assert.equal(f.frames.size,0);
  }
});
