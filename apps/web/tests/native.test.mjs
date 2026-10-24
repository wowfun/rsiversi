import assert from 'node:assert/strict';
import {test} from 'node:test';
import {desktopCall,saveDesktopExport} from '../desktop-bridge.js';
import {readFileSync} from 'node:fs';
import {desktopBridgeDescriptor} from '../wire-descriptors.mjs';
const descriptor = desktopBridgeDescriptor();

test('document encodes the shared native request corpus with its actual build descriptor', async () => {
  const cases = JSON.parse(readFileSync(new URL('../../../crates/rsi/desktop-bridge/conformance.json', import.meta.url), 'utf8'));
  const protocol = desktopBridgeDescriptor();
  for (const sample of cases.filter(sample => sample.valid)) {
    let request;
    const scope = {__RSI_DESKTOP__:true, __TAURI_INTERNALS__:{invoke: async (_command, {payload}) => { request = JSON.parse(payload); }}};
    await desktopCall(sample.operation, scope, protocol);
    assert.deepEqual(request, {protocol, operation:sample.operation}, sample.name);
  }
});
test('native call sends the explicit descriptor and operation to a single product command',async()=>{
  const calls=[],scope={__RSI_DESKTOP__:true,__TAURI_INTERNALS__:{invoke:(...args)=>{calls.push(args);return Promise.resolve(null);}}};
  await desktopCall({kind:'clipboard',text:'hello'},scope,descriptor);
  assert.deepEqual(calls,[['rsi_bridge',{payload:JSON.stringify({protocol:descriptor,operation:{kind:'clipboard',text:'hello'}})}]]);
  await assert.rejects(desktopCall({kind:'hello'},{},descriptor),/unavailable/);
});
for(const corrupt of [false,true]) test(`Desktop export ${corrupt?'rejects trailing data':'streams through verified completion'}`,async()=>{
  const calls=[],events=[{type:'chunk',offset:'0',text:'hello'},{type:'complete',bytes:'5',sha256:'digest'},corrupt?{type:'chunk',text:'bad'}:null];
  globalThis.__RSI_DESKTOP__=true;globalThis.__RSI_DESKTOP_BRIDGE__=descriptor;
  globalThis.__TAURI_INTERNALS__={invoke:async(command,{payload})=>{const op=JSON.parse(payload).operation;calls.push(op);return op.kind==='export_open'?{token:'b'.repeat(64)}:op.kind==='export_finish'?{bytes:'5'}:null;}};
  const call=async(method,input)=>{assert.equal(method,'export_input');const op=JSON.parse(input).operation;return JSON.stringify(op.kind==='open'?{token:'worker',filename:'test.md',event:{type:'start'}}:op.kind==='next'?events.shift():null);};
  try {
    const promise=saveDesktopExport(call,1,'generation',{},new AbortController().signal);
    if(corrupt) {await assert.rejects(promise,/continued/);assert.equal(calls.at(-1).kind,'export_cancel');assert.ok(!calls.some(op=>op.kind==='export_finish'));}
    else {assert.deepEqual(await promise,{bytes:'5'});assert.deepEqual(calls.map(op=>op.kind),['export_open','export_write','export_write','export_finish']);}
  } finally {delete globalThis.__RSI_DESKTOP__;delete globalThis.__RSI_DESKTOP_BRIDGE__;delete globalThis.__TAURI_INTERNALS__;}
});
