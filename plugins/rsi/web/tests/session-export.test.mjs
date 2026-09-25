import {test} from 'node:test';
import assert from 'node:assert/strict';
import {downloadSession} from '../session-export.js';

globalThis.location = {protocol:'rsi:'};
const deferred = () => { let resolve; const promise = new Promise(r=>resolve=r); return {promise,resolve}; };

test('abort while reservation is pending cancels the returned token before save', async()=>{
  const opened=deferred(), controller=new AbortController(), calls=[];
  const result=downloadSession(async(method,body)=>{
    calls.push([method,body]);
    if(method==='export_open') return opened.promise;
    assert.equal(method,'export_cancel'); assert.deepEqual(JSON.parse(body),{token:'A'});
    return 'null';
  },'main',7,'latest',controller.signal);
  controller.abort(); opened.resolve('{"token":"A"}');
  await assert.rejects(result,/Export cancelled/);
  assert.deepEqual(calls.map(([method])=>method),['export_open','export_cancel']);
});

test('cancellation retries only positively classified pre-admission failures',async()=>{
  for(const retryable of [true,false]) {
    const saving=deferred(), controller=new AbortController(); let attempts=0;
    const failure=Object.assign(new Error('control failed'),{notAdmitted:retryable,retryable});
    const result=downloadSession(async(method,body)=>{
      if(method==='export_open') return '{"token":"B"}';
      assert.equal(JSON.parse(body).token,'B');
      if(method==='export_save') { controller.abort(); await saving.promise; throw new Error('Export cancelled'); }
      attempts++;
      if(attempts===1) { if(!retryable) saving.resolve(); throw failure; }
      saving.resolve(); return 'null';
    },'main',3,'-f json',controller.signal);
    await assert.rejects(result,retryable?/Export cancelled/:/control failed/);
    assert.equal(attempts,retryable?2:1);
  }
});

test('confirmed replacement stays successful after cancellation and failed control delivery',async()=>{
  const controller=new AbortController();
  const result=await downloadSession(async(method,body)=>{
    if(method==='export_open') return '{"token":"C"}';
    if(method==='export_cancel') throw new Error('connection closed');
    assert.deepEqual(JSON.parse(JSON.parse(body).source),{pane:'main',generation:1,arguments:'-f json'});
    controller.abort(); return '{"filename":"saved.json"}';
  },'main',1,'-f json',controller.signal);
  assert.deepEqual(result,{filename:'saved.json'});
});
