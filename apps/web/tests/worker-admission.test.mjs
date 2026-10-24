import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {lane,limits} from '../admission.js';

test('the real Worker forwards Rust command admission metadata without classifying outcome errors',async()=>{
  const source=(await readFile(new URL('../worker.js',import.meta.url),'utf8')).replace(/^import .*;\r?\n/gm,'');
  const replies=[];let failure=Object.assign(new Error('busy'),{notAdmitted:true});
  const context={lane,limits,self:{},postMessage:value=>replies.push(value),init:async()=>{},build_family:()=> '__RSI_BUILD_FAMILY__',
    command:async()=>{throw failure},connect:async()=> '{}',next_view:()=>new Promise(()=>{}),setTimeout,clearTimeout,Uint8Array};
  vm.runInNewContext(source,context);
  context.self.onmessage({data:{kind:'call',id:1,method:'connect',payload:{buildFamily:'__RSI_BUILD_FAMILY__'}}});
  await new Promise(resolve=>setImmediate(resolve));
  context.self.onmessage({data:{kind:'call',id:2,method:'command',payload:'{}'}});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(replies.find(r=>r.id===2).notAdmitted,true);
  assert.equal(replies.find(r=>r.id===2).retryable,true);
  failure=new Error('unknown admitted outcome');
  context.self.onmessage({data:{kind:'call',id:3,method:'command',payload:'{}'}});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(replies.find(r=>r.id===3).notAdmitted,false);
  assert.equal(replies.find(r=>r.id===3).retryable,false);
  failure=Object.assign(new Error('closed'),{notAdmitted:true,retryable:false});
  context.self.onmessage({data:{kind:'call',id:4,method:'command',payload:'{}'}});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(replies.find(r=>r.id===4).retryable,false);
});
