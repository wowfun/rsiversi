import test from "node:test";
import assert from "node:assert/strict";
import {DocumentConnection} from "../document-connection.js";
import {createDocumentClose} from '../src/runtime/document-close.js';
function fixture(connection) {
 const calls=[],failures=[];
 const context={connection,panes:new Map(),notify(){},desktopCall:async operation=>{calls.push(operation)},failWorker:message=>{failures.push(message)},$:()=>({})};
 const closer=createDocumentClose({get connection(){return context.connection},get panes(){return context.panes},get nativeCall(){return context.desktopCall},nativeDocument:true,notify:context.notify,failWorker:context.failWorker,setClosing(value){context.$('sign-out').disabled=value},disconnected(){}});
 Object.assign(context,closer);
 Object.defineProperty(context,'nativeCloseAttempt',{get:()=>closer.attempt,set:value=>{closer.attempt=value}});
 context.nativeCloseAttempt=1;
 return {context,calls,failures};
}
for (const phase of ["Draining", "Failed"]) test(`native close waits for ${phase} cleanup and acknowledges once`, async () => {
  let release;
  const cleanup = new Promise(resolve => {release = resolve;});
  const owner = {phase, closing: true, settled: () => cleanup};
  const {context, calls} = fixture(owner);
  const closing = context.disconnectDocument();
  await Promise.resolve(); assert.equal(calls.length, 0);
  owner.phase = phase === "Draining" ? "Closed" : "Failed";
  release(); await closing;
  assert.equal(calls.length, 1); assert.equal(calls[0].ready, true);
  assert.equal(context.nativeCloseAttempt, undefined);
  await context.disconnectDocument(); assert.equal(calls.length, 1);
});
test("bridge rejection consumes only the rejected close attempt", async () => {
  const {context} = fixture({phase: "Closed"});
  context.desktopCall = async () => {context.nativeCloseAttempt = 2; throw new Error("bridge failed");};
  await assert.rejects(context.disconnectDocument(), /bridge failed/);
  assert.equal(context.nativeCloseAttempt, 2);
});
test("failed native cancellation clears the stale identity and reports the draft failure", async () => {
  const owner = {phase: "Ready", closing: false};
  const {context, failures} = fixture(owner);
  context.desktopCall = async () => {throw new Error("bridge failed");};
  assert.equal(await context.cancelNativeCloseAfterDraftFailure(owner, new Error("unsaved")), false);
  assert.equal(context.nativeCloseAttempt, undefined);
  assert.match(failures[0], /Draft is not saved: unsaved/);
});

test("transport failure during disconnect waits for cleanup then acknowledges", async () => {
  let release;
  const cleanup = new Promise(resolve => {release = resolve;});
  const owner = {phase: "Ready", closing: false, settled: () => cleanup,
    disconnect: async () => {owner.phase = "Failed";}};
  const {context, calls} = fixture(owner);
  context.$ = () => ({}); context.panes = new Map();
  const closing = context.disconnectDocument();
  await Promise.resolve(); assert.equal(calls.length, 0);
  release(); await closing;
  assert.equal(calls.length, 1); assert.equal(calls[0].ready, true);
  assert.equal(context.nativeCloseAttempt, undefined);
});

test("real DocumentConnection failure during draft flush still completes native close", async () => {
  let flush, cleanup;
  const flushing = new Promise(resolve => {flush = resolve;});
  const cleaning = new Promise(resolve => {cleanup = resolve;});
  const transport = {terminate: () => cleaning, postMessage(message) {
    queueMicrotask(() => this.onmessage({data:{kind:"reply", id:message.id, result:"{}"}}));
  }};
  const owner = new DocumentConnection({transport, mounts:{close(){}}, isCurrent:()=>true, onFailure(){}});
  await owner.authenticate({}, async()=>({}));
  const {context, calls} = fixture(owner);
  context.$=()=>({}); context.panes=new Map([["main",{flush:()=>flushing}]]);
  const closing=context.disconnectDocument();
  await transport.onmessage({data:{kind:"failed",error:"fixture transport failure"}});
  flush(); await Promise.resolve(); assert.equal(calls.length,0);
  cleanup(); await closing;
  assert.equal(owner.phase,"Failed");
  assert.deepEqual(calls.map(call=>[call.kind,call.attempt,call.ready]), [["close",1,true]]);
});

test("Host loss after Worker failure waits for local draft persistence before native exit", async () => {
  let saved; const pending = new Promise(resolve => {saved=resolve;});
  const {context,calls} = fixture({phase:"Failed",closing:true,settled:async()=>{}});
  context.panes.set("main",{flush:()=>pending});
  const closing=context.disconnectDocument();
  await Promise.resolve(); assert.equal(calls.length,0);
  saved(); await closing;
  assert.equal(calls[0].ready,true);
});
test("failed local draft persistence after Host loss cancels native exit", async () => {
  const {context,calls} = fixture({phase:"Failed",closing:true,settled:async()=>{}});
  context.panes.set("main",{flush:async()=>{throw new Error("disk full");}});
  await context.disconnectDocument();
  assert.deepEqual(calls.map(call=>call.ready),[false]);
});

test('stalled draft persistence after Host loss cancels native exit at the stage deadline', async t=>{
  t.mock.timers.enable({apis:['setTimeout']});
  const {context,calls}=fixture({phase:'Failed',closing:true,settled:async()=>{}});
  context.panes.set('main',{flush:()=>new Promise(()=>{})});
  const closing=context.disconnectDocument();
  t.mock.timers.tick(30_000);
  await closing;
  assert.deepEqual(calls.map(call=>call.ready),[false]);
});

test("Worker failure during a rejected draft flush still cancels native exit", async () => {
  let reject;
  const failed = new Promise((_,no)=>{reject=no;});
  const transport={terminate(){},postMessage(message){queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:"{}"}}));}};
  const owner=new DocumentConnection({transport,mounts:{close(){}},isCurrent:()=>true,onFailure(){}});
  await owner.authenticate({},async()=>({}));
  const {context,calls}=fixture(owner);context.$=()=>({});context.panes.set("main",{flush:()=>failed});
  const closing=context.disconnectDocument();
  await transport.onmessage({data:{kind:"failed",error:"Host disconnected"}});
  reject(new Error("disk full"));await closing;
  assert.deepEqual(calls.map(call=>call.ready),[false]);
});


test('overlapping close requests acknowledge every identity exactly once after cleanup', async()=>{
  let release; const cleaning=new Promise(resolve=>{release=resolve});
  const owner={phase:'Draining',closing:true,settled:()=>cleaning};
  const {context,calls}=fixture(owner);
  const first=context.disconnectDocument();
  context.nativeCloseAttempt=2;
  const second=context.disconnectDocument();
  await Promise.resolve();assert.equal(calls.length,0);
  owner.phase='Closed';release();await Promise.all([first,second]);
  assert.deepEqual(calls.map(call=>[call.attempt,call.ready]),[[1,true],[2,true]]);
  await context.disconnectDocument();assert.equal(calls.length,2);
});
