import assert from 'node:assert/strict';
import {test} from 'node:test';
import {setImmediate} from 'node:timers/promises';
import {NativeDocument} from '../src/native.ts';
import {TerminalInput} from '../src/terminal-input.mjs';

test('native admission rejection preserves queued input through the reply contract', async () => {
  const fetch = globalThis.fetch;
  let attempts = 0;
  const delivered = [], failures = [];
  globalThis.fetch = async (_path, init) => {
    if (++attempts === 1) return Response.json({code:'busy',message:'busy',notAdmitted:true,retryable:true},{status:409});
    delivered.push(JSON.parse(init.body));
    return new Response('ok');
  };
  try {
    const native = new NativeDocument();
    const pending = new Map(); let id = 0;
    native.onmessage = ({data}) => {
      const waiter = pending.get(data.id); pending.delete(data.id);
      if (data.error) waiter.reject(Object.assign(Error(data.error),{notAdmitted:data.notAdmitted===true,retryable:data.retryable!==false}));
      else waiter.resolve(data.result);
    };
    const pump = new TerminalInput(bytes => new Promise((resolve,reject) => {
      pending.set(++id,{resolve,reject});
      native.postMessage({kind:'call',id,method:'terminal',payload:JSON.stringify([...bytes])});
    }), message => failures.push(message), async()=>{});
    pump.push(Uint8Array.of(65)); pump.push(Uint8Array.of(66));
    while(pump.working) await setImmediate();
    assert.deepEqual(delivered.flat(),[65,66]); assert.equal(attempts,3);
    assert.deepEqual(failures,[]); assert.equal(pump.stopped,false);
  } finally { globalThis.fetch = fetch; }
});

test('native only trusts the exact rejection classification', async () => {
  const fetch = globalThis.fetch;
  try {
    for (const [body,notAdmitted] of [
      [{code:'closed',message:'closed',notAdmitted:true,retryable:false},true],
      [{code:'invalid',message:'invalid',notAdmitted:true,retryable:false},true],
      [{code:'failed',message:'uncertain',notAdmitted:false,retryable:false},false],
      [{code:'failed',message:'uncertain',notAdmitted:true,retryable:true},false],
      [{code:'busy',message:'busy',notAdmitted:true,retryable:false},false],
      [{code:'other',message:'unknown',notAdmitted:true,retryable:true},false],
      ['plain error',false],
    ]) {
      globalThis.fetch=async()=>Response.json(body,{status:409});
      const native=new NativeDocument();
      const reply=await new Promise(resolve=>{native.onmessage=({data})=>resolve(data);native.postMessage({kind:'call',id:1,method:'command',payload:'{}'});});
      assert.equal(reply.notAdmitted===true,notAdmitted);assert.equal(reply.retryable===true,false);
    }
    globalThis.fetch=async()=>{throw Error('connection lost');};
    const native=new NativeDocument();
    const reply=await new Promise(resolve=>{native.onmessage=({data})=>resolve(data);native.postMessage({kind:'call',id:1,method:'terminal',payload:'{}'});});
    assert.equal(reply.notAdmitted===true,false);
  } finally { globalThis.fetch=fetch; }
});

test('native delivers decoded frames and preserves exact acknowledgement identity',async()=>{
  const fetch=globalThis.fetch, requests=[];
  const view={frame_id:'9007199254740993',kind:'snapshot'}, assets={revision:'a'.repeat(64),catalog:null};
  const native=new NativeDocument();
  let finish;const done=new Promise(resolve=>finish=resolve);
  globalThis.fetch=async(path,init)=>{
    requests.push([path,init.body]);
    if(path.startsWith('/_frame'))return Response.json({view,assets});
    if(path==='/_ack'){native.terminate();finish();}
    return new Response('{}');
  };
  try {
    native.onmessage=({data})=>{
      if(data.kind==='view'){
        assert.deepEqual(data.view,view);assert.deepEqual(data.assets,assets);
        native.postMessage({kind:'ack',frame_id:view.frame_id,resync:true,renderer:{revision:assets.revision,accept:true}});
      }
    };
    native.postMessage({kind:'call',id:1,method:'connect',payload:{}});
    await done;
    assert.deepEqual(JSON.parse(requests.find(([path])=>path==='/_ack')[1]),{frame_id:view.frame_id,renderer:{revision:assets.revision,accept:true}});
  }finally{globalThis.fetch=fetch;}
});
