import {afterEach, beforeEach, test} from 'node:test';
import assert from 'node:assert/strict';
import {desktopBridgeDescriptor} from '../wire-descriptors.mjs';
import {downloadSession} from '../session-export.js';

const deferred = () => { let resolve; const promise = new Promise(r=>resolve=r); return {promise,resolve}; };
let nativeCalls, original;
beforeEach(() => {
  original = Object.fromEntries(['__RSI_DESKTOP__','__RSI_DESKTOP_BRIDGE__','__TAURI_INTERNALS__'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
  nativeCalls = [];
  globalThis.__RSI_DESKTOP__ = true;
  globalThis.__RSI_DESKTOP_BRIDGE__ = desktopBridgeDescriptor();
  globalThis.__TAURI_INTERNALS__ = {invoke:async (_command,{payload})=>{
    const operation=JSON.parse(payload).operation; nativeCalls.push(operation);
    return operation.kind==='export_open'?{token:'native'}:null;
  }};
});
afterEach(() => {
  for (const [key,descriptor] of Object.entries(original)) {
    if(descriptor) Object.defineProperty(globalThis,key,descriptor); else delete globalThis[key];
  }
});
const worker = handler => async (method, body) => {
  assert.equal(method,'export_input');
  const input=JSON.parse(body);
  assert.equal(input.pane,'main');
  assert.equal(input.generation,7);
  return JSON.stringify(await handler(input.operation));
};

test('abort during Worker reservation cancels the returned token before native admission',async()=>{
  const opened=deferred(),controller=new AbortController(),calls=[];
  const result=downloadSession(worker(async operation=>{
    calls.push(operation);
    if(operation.kind==='open') return opened.promise;
    assert.deepEqual(operation,{kind:'cancel',token:'worker'});
    return null;
  }),'main',7,'latest',controller.signal);
  controller.abort(); opened.resolve({token:'worker',event:{type:'start'}});
  await assert.rejects(result,/Export cancelled/);
  assert.deepEqual(calls.map(operation=>operation.kind),['open','cancel']);
  assert.deepEqual(nativeCalls,[]);
});

test('abort during the native save dialog cancels its later returned reservation',async()=>{
  const admitted=deferred(),dialog=deferred(),controller=new AbortController(),calls=[];
  globalThis.__TAURI_INTERNALS__.invoke=async (_command,{payload})=>{
    const operation=JSON.parse(payload).operation;nativeCalls.push(operation);
    if(operation.kind==='export_open') {admitted.resolve();return dialog.promise;}
    return null;
  };
  const result=downloadSession(worker(async operation=>{
    calls.push(operation);
    if(operation.kind==='open') return {token:'worker',event:{type:'start'}};
    assert.equal(operation.kind,'cancel');return null;
  }),'main',7,'latest',controller.signal);
  await admitted.promise;
  controller.abort();dialog.resolve({token:'native'});
  await assert.rejects(result,/Export cancelled/);
  assert.deepEqual(nativeCalls.map(operation=>operation.kind),['export_open','export_cancel']);
  assert.ok(calls.some(operation=>operation.kind==='cancel'));
});

test('confirmed native replacement stays successful after abort and failed Worker cleanup',async()=>{
  const controller=new AbortController();
  const events=[{type:'chunk',offset:'0',text:'saved'},{type:'complete',bytes:'5',sha256:'digest'},null];
  globalThis.__TAURI_INTERNALS__.invoke=async (_command,{payload})=>{
    const operation=JSON.parse(payload).operation;nativeCalls.push(operation);
    if(operation.kind==='export_open') return {token:'native'};
    if(operation.kind==='export_finish') {controller.abort();return {filename:'saved.json'};}
    return null;
  };
  const result=await downloadSession(worker(async operation=>{
    if(operation.kind==='open') return {token:'worker',event:{type:'start'}};
    if(operation.kind==='cancel') throw new Error('Worker connection closed');
    assert.equal(operation.kind,'next');return events.shift();
  }),'main',7,'-f json',controller.signal);
  assert.deepEqual(result,{filename:'saved.json'});
  assert.equal(nativeCalls.filter(operation=>operation.kind==='export_finish').length,1);
});

test('a failed source stream cancels native staging without committing it',async()=>{
  const controller=new AbortController();
  await assert.rejects(downloadSession(worker(async operation=>{
    if(operation.kind==='open') return {token:'worker',event:{type:'start'}};
    if(operation.kind==='cancel') return null;
    throw new Error('source stream failed');
  }),'main',7,'latest',controller.signal),/source stream failed/);
  assert.deepEqual(nativeCalls.map(operation=>operation.kind),['export_open','export_cancel']);
});
