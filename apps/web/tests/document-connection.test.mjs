import test from "node:test";
import assert from "node:assert/strict";
import { DocumentConnection } from "../document-connection.js";
import { NativeDocument } from "../src/native.ts";

const gate = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
function openingFixture(options = {}) {
  let current;
  const calls = [], failures = [];
  const transport = { terminated: 0, postMessage(message) {
    calls.push(message);
    if (message.method === "connect" || message.method === "disconnect") queueMicrotask(() => this.onmessage({data:{kind:"reply",id:message.id,result:message.method === "connect" ? options.identity??'{"endpoint_id":"fixture"}' : {active_requests:0}}}));
  }, terminate() { this.terminated++; return options.terminate?.(); } };
  const mounts = { closed: 0, render() {}, close() { this.closed++; return options.close?.(); } };
  current = new DocumentConnection({transport,mounts,isCurrent:()=>current===owner,onFrame:options.onFrame??(()=>({accepted:true})),onFailure:error=>failures.push(error.message)});
  const owner = current;
  return {owner,transport,mounts,calls,failures,replace(value){current=value;}};
}
async function fixture(options = {}) {
  const f = openingFixture(options);
  await f.owner.authenticate({}, options.openDrafts??(async()=>({})));
  return f;
}

for(const reason of [new Error("storage setup failed"),"storage setup failed"]) test(`authentication storage ${typeof reason} rejection fences waiting frames and pending input`,async()=>{
  const opening=gate(),storage=gate();let frames=0;
  const f=openingFixture({onFrame:()=>{frames++;return {accepted:true};}});
  const authentication=f.owner.authenticate({},()=>{opening.resolve();return storage.promise;});
  const rejected=assert.rejects(authentication,/storage setup failed/);
  await opening.promise;
  const pending=f.owner.request("command","{}").catch(error=>error);
  const frame=f.transport.onmessage({data:{kind:"view",view:{frame_id:"opening"},assets:{}}});
  storage.reject(reason);
  await rejected;await frame;
  try {
    assert.equal(f.owner.phase,"Failed");
    assert.equal(frames,0);
    assert.deepEqual(f.failures,["storage setup failed"]);
    assert.equal(f.calls.filter(call=>call.kind==="ack").length,0);
    await assert.rejects(f.owner.request("command","{}"),error=>error.notAdmitted===true);
    assert.match((await pending).message,/storage setup failed/);
    await f.owner.settled();
    assert.equal(f.mounts.closed,1);assert.equal(f.transport.terminated,1);
  } finally {await f.owner.retire();}
});

test("invalid authentication JSON fails and closes the opening owner",async()=>{
  const f=openingFixture({identity:"{"});let opened=0;
  await assert.rejects(f.owner.authenticate({},()=>{opened++;return {};}),SyntaxError);
  await f.owner.settled();
  assert.equal(f.owner.phase,"Failed");assert.equal(opened,0);
  assert.equal(f.failures.length,1);assert.equal(f.transport.terminated,1);assert.equal(f.mounts.closed,1);
});

for (const saveFails of [false, true]) test(`late old draft ${saveFails?'failure':'success'} cannot close or revive a replacement`, async () => {
  const save = gate(), old = await fixture();
  const closing = old.owner.disconnect(()=>save.promise);
  assert.equal(old.owner.phase,"Draining");
  old.transport.onerror({preventDefault(){}});
  await old.owner.settled();
  const fresh = await fixture(); old.replace(fresh.owner);
  if(saveFails)save.reject(new Error("old save failed"));else save.resolve();
  assert.equal(await closing,undefined);
  assert.equal(old.owner.phase,"Failed");
  assert.equal(fresh.owner.phase,"Ready");
  assert.equal(fresh.transport.terminated,0);
  assert.equal(fresh.mounts.closed,0);
  assert.equal(old.calls.filter(call=>call.method==="disconnect").length,0);
});

test("disconnect captures admission and orders draft, renderer, receipt and termination",async()=>{
  const save=gate(),dispose=gate(),termination=gate(),disposing=gate(),terminating=gate();
  const f=await fixture({close:()=>{disposing.resolve();return dispose.promise;},terminate:()=>{terminating.resolve();return termination.promise;}});
  const pending=f.owner.request("command","{}");
  const rejected=assert.rejects(pending,/disconnected/);
  const closing=f.owner.disconnect(()=>save.promise);
  let duplicateFlushes=0;const duplicate=f.owner.disconnect(()=>{duplicateFlushes++;});
  await assert.rejects(f.owner.request("command","{}"),error=>error.notAdmitted===true);
  assert.equal(f.mounts.closed,0);
  save.resolve();await disposing.promise;
  assert.equal(f.mounts.closed,1);
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,0);
  dispose.resolve();
  await terminating.promise;
  assert.equal(f.transport.terminated,1);
  assert.equal(f.owner.phase,"Draining");
  termination.resolve();assert.deepEqual(await closing,{active_requests:0});
  assert.deepEqual(await duplicate,{active_requests:0});assert.equal(duplicateFlushes,0);await rejected;
  assert.equal(f.owner.phase,"Closed");assert.equal(f.mounts.closed,1);
});

test("authentication storage cannot replace connection operations",async()=>{
  const drafts={},f=await fixture({openDrafts:async()=>({drafts,storageNotice:"quota",request:()=>"shadowed",retire:()=>"shadowed"})});
  assert.equal(f.owner.drafts,drafts);assert.equal(f.owner.storageNotice,"quota");
  const pending=f.owner.request("command","{}");
  assert.equal(f.calls.at(-1).method,"command");
  const rejected=assert.rejects(pending,/replaced/);
  await f.owner.retire();await rejected;
  assert.equal(f.transport.terminated,1);assert.equal(f.mounts.closed,1);
});

for(const failure of ["response","network"]) test(`native ${failure} cleanup failure remains observable and blocks replacement`,async()=>{
  const original=globalThis.fetch;let calls=0;
  globalThis.fetch=async path=>{assert.equal(path,"/_failed");calls++;if(failure==="network")throw new Error("offline");return new Response("cleanup incomplete",{status:503});};
  try {
    const native=new NativeDocument(),f=await fixture({terminate:()=>native.terminate()});
    const matches=failure==="network"?/offline/:/cleanup failed/;
    await assert.rejects(f.owner.retire(),matches);
    assert.equal(f.owner.phase,"Closed");
    await assert.rejects(f.owner.settled(),matches);
    await assert.rejects(f.owner.retire(),matches);
    await assert.rejects(f.owner.request("command","{}"),error=>error.notAdmitted===true);
    assert.equal(calls,1);assert.equal(f.mounts.closed,1);
    assert.throws(()=>native.postMessage({kind:"call"}),/closed/);
  } finally {globalThis.fetch=original;}
});

test("failed save restores only the current draining owner",async()=>{
  const f=await fixture();
  await assert.rejects(f.owner.disconnect(()=>Promise.reject(new Error("save failed"))),error=>error.draftSaveFailed===true);
  assert.equal(f.owner.phase,"Ready");assert.equal(f.mounts.closed,0);
  assert.deepEqual(await f.owner.disconnect(async()=>{}),{active_requests:0});
});

test("old frame failure and replies settle only their original generation",async()=>{
  const render=gate(),f=await fixture({onFrame:()=>render.promise});
  let settled=0;const waiting=f.owner.request("command","{}").catch(()=>{settled++;});
  const frame=f.transport.onmessage({data:{kind:"view",view:{frame_id:"1"},assets:{}}});
  await Promise.resolve();f.owner.fail(new Error("lost"));await f.owner.settled();
  const fresh=await fixture();f.replace(fresh.owner);
  render.reject(new Error("old renderer failed"));await frame;await waiting;
  await f.transport.onmessage({data:{kind:"reply",id:2,result:"late"}});
  f.owner.fail(new Error("again"));
  assert.equal(settled,1);assert.equal(f.transport.terminated,1);assert.equal(f.mounts.closed,1);
  assert.equal(fresh.transport.terminated,0);assert.equal(f.failures.length,1);
});

test("lane limits refuse before transport and cleanup failure remains observable",async()=>{
  const f=await fixture({close:()=>Promise.reject(new Error("dispose failed"))});
  const pending=Array.from({length:8},()=>f.owner.request("command","{}").catch(()=>{}));
  await assert.rejects(f.owner.request("command","{}"),error=>error.notAdmitted===true);
  assert.equal(f.calls.filter(call=>call.method==="command").length,8);
  f.owner.fail(new Error("lost"));await Promise.all(pending);
  await assert.rejects(f.owner.settled(),/dispose failed/);
});

test("native termination is idempotent and waits for the actual close response",async()=>{
  const original=globalThis.fetch,closed=gate();let calls=0;
  globalThis.fetch=async path=>{assert.equal(path,"/_failed");calls++;return closed.promise;};
  try {
    const native=new NativeDocument();
    const first=native.terminate();assert.equal(native.terminate(),first);assert.equal(calls,1);
    let settled=false;first.then(()=>{settled=true;});await Promise.resolve();assert.equal(settled,false);
    closed.resolve({ok:true});await first;assert.equal(settled,true);
    assert.throws(()=>native.postMessage({kind:"call"}),/closed/);
  } finally {globalThis.fetch=original;}
});

test("transport failure while draining rejects pending replies and finishes the original owner", async () => {
  const save = gate(), f = await fixture();
  const pending = assert.rejects(f.owner.request("command", "{}"), /lost while draining/);
  const closing = f.owner.disconnect(() => save.promise);
  await f.transport.onmessage({data:{kind:"failed",error:"lost while draining"}});
  await pending;
  assert.equal(f.owner.phase, "Failed");
  await f.owner.settled();
  assert.equal(f.transport.terminated, 1);
  save.resolve();
  await closing;
  assert.equal(f.calls.filter(call => call.method === "disconnect").length, 0);
});

for (const reason of ["quota exhausted", null]) test(`non-Error draft rejection ${String(reason)} keeps a useful message`, async () => {
  const f = await fixture();
  await assert.rejects(f.owner.disconnect(() => Promise.reject(reason)), error =>
    error instanceof Error && error.message === (reason === null ? "Draft save failed" : reason) && error.draftSaveFailed === true);
  assert.equal(f.owner.phase, "Ready");
});

for (const saveFails of [false, true]) test(`replacement retires a draining owner before late draft ${saveFails ? "failure" : "success"}`, async () => {
  const save = gate(), dispose = gate(), old = await fixture({close: () => dispose.promise});
  const closing = old.owner.disconnect(() => save.promise);
  const retirement = old.owner.retire();
  assert.equal(old.owner.phase, "Closed");
  assert.equal(old.transport.terminated, 1);
  assert.equal(old.mounts.closed, 1);
  let retired = false; retirement.then(() => {retired = true;});
  await Promise.resolve(); assert.equal(retired, false);
  dispose.resolve(); await retirement;
  const fresh = await fixture(); old.replace(fresh.owner);
  if (saveFails) save.reject(new Error("late save failed")); else save.resolve();
  await closing;
  assert.equal(old.mounts.closed, 1);
  assert.equal(old.transport.terminated, 1);
  assert.equal(fresh.owner.phase, "Ready");
  assert.equal(fresh.transport.terminated, 0);
  assert.equal(old.calls.filter(call => call.method === "disconnect").length, 0);
});

test("settled waits for a live drain and follows completed failure cleanup", async () => {
  const save = gate(), f = await fixture();
  const closing = f.owner.disconnect(() => save.promise);
  let settled = false; const settlement = f.owner.settled().then(() => {settled = true;});
  await Promise.resolve(); assert.equal(settled, false);
  save.resolve(); await closing; await settlement;
  assert.equal(settled, true); assert.equal(f.owner.phase, "Closed");
});

test("close during opening waits for storage then drains the same owner", async () => {
  const storage = gate(), entered = gate(), f = openingFixture();
  const authenticating = f.owner.authenticate({}, async () => { entered.resolve(); return storage.promise; });
  await entered.promise;
  let flushes = 0;
  const closing = f.owner.disconnect(async () => { flushes++; });
  const duplicate = f.owner.disconnect(() => assert.fail("duplicate flush"));
  let settled = false;
  const settlement = f.owner.settled().then(() => { settled = true; });
  await Promise.resolve();
  assert.equal(settled, false, "Opening still owes storage, drain and receipt");
  storage.resolve({drafts:{owner:"opening"}});
  await authenticating;
  assert.deepEqual(await closing, {active_requests:0});
  await settlement;
  assert.equal(settled, true);
  assert.equal(duplicate, closing);
  assert.equal(flushes,1);
  assert.equal(f.owner.phase,"Closed");
  assert.equal(f.transport.terminated,1);
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,1);
});

test("close during failed authentication waits for actual termination", async () => {
  const storage=gate(),entered=gate(),termination=gate(),terminating=gate();
  const f=openingFixture({terminate:()=>{terminating.resolve();return termination.promise;}});
  const authentication=f.owner.authenticate({},()=>{entered.resolve();return storage.promise;});
  const rejected=assert.rejects(authentication,/storage failed/);
  await entered.promise;
  const closing=f.owner.disconnect(()=>assert.fail("failed authentication must not flush"));
  let settled=false;void closing.then(()=>{settled=true;});
  storage.reject(new Error("storage failed"));await rejected;await terminating.promise;
  assert.equal(settled,false);termination.resolve();await closing;
  assert.equal(f.owner.phase,"Failed");assert.equal(f.transport.terminated,1);
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,0);
});

test("replacement during opening close retires only the captured owner", async () => {
  const storage=gate(),entered=gate(),old=openingFixture();
  const authentication=old.owner.authenticate({},()=>{entered.resolve();return storage.promise;});
  await entered.promise;
  const closing=old.owner.disconnect(()=>assert.fail("retired owner must not flush"));
  const fresh=await fixture();old.replace(fresh.owner);
  await old.owner.retire();storage.resolve({});await authentication;await closing;
  assert.equal(old.transport.terminated,1);assert.equal(old.mounts.closed,1);
  assert.equal(old.calls.filter(call=>call.method==="disconnect").length,0);
  assert.equal(fresh.owner.phase,"Ready");assert.equal(fresh.transport.terminated,0);
  await fresh.owner.retire();
});

for (const accepted of [true,false]) test(`presentation ACK preserves renderer and resync when accepted=${accepted}`, async () => {
  const renderer = {fixture:"renderer"}, f = await fixture({onFrame:async()=>({accepted,renderer})});
  await f.transport.onmessage({data:{kind:"view",view:{frame_id:17},assets:{}}});
  assert.deepEqual(f.calls.at(-1),{kind:"ack",frame_id:17,resync:!accepted,renderer});
  await f.owner.retire();
});

test("overlapping presentation fails once and cannot ACK a late frame", async () => {
  const presented = gate(), f = await fixture({onFrame:()=>presented.promise});
  const first = f.transport.onmessage({data:{kind:"view",view:{frame_id:1},assets:{}}});
  await f.transport.onmessage({data:{kind:"view",view:{frame_id:2},assets:{}}});
  presented.resolve({accepted:true});await first;await f.owner.settled();
  assert.deepEqual(f.failures,["Overlapping presentation frames"]);
  assert.equal(f.calls.filter(call=>call.kind==="ack").length,0);
});

test("draft-save deadline keeps recovery available and late save cannot disconnect", async t => {
  t.mock.timers.enable({apis:["setTimeout"]});
  const save=gate(),f=await fixture();
  const closing=f.owner.disconnect(()=>save.promise);
  const rejected=assert.rejects(closing,error=>error.draftSaveFailed===true&&/Draft save/.test(error.message));
  t.mock.timers.tick(29_999);
  assert.equal(f.owner.phase,"Draining");
  t.mock.timers.tick(1);await rejected;
  assert.equal(f.owner.phase,"Ready");assert.equal(f.mounts.closed,0);
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,0);
  save.resolve();await save.promise;await Promise.resolve();
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,0);
  assert.deepEqual(await f.owner.disconnect(async()=>{}),{active_requests:0});
});

for(const stage of ["renderer","transport"]) test(`${stage} cleanup deadline stays failed after late completion`,async t=>{
  t.mock.timers.enable({apis:["setTimeout"]});
  const blocked=gate(),entered=gate();
  const stall=()=>{entered.resolve();return blocked.promise;};
  const f=await fixture(stage==="renderer"?{close:stall}:{terminate:stall});
  const retirement=f.owner.retire();
  const rejected=assert.rejects(retirement,/did not complete within 30 seconds/);
  await entered.promise;t.mock.timers.tick(30_000);await rejected;
  await assert.rejects(f.owner.settled(),/did not complete/);
  blocked.resolve();await blocked.promise;
  await assert.rejects(f.owner.retire(),/did not complete/);
  assert.equal(f.owner.phase,"Closed");assert.equal(f.transport.terminated,1);
  await assert.rejects(f.owner.request("command","{}"),error=>error.notAdmitted===true);
});

test("opening close deadline fences late storage and frames without issuing a receipt",async t=>{
  t.mock.timers.enable({apis:["setTimeout"]});
  const storage=gate(),entered=gate();let frames=0;
  const f=openingFixture({onFrame:()=>{frames++;}});
  const authentication=f.owner.authenticate({},()=>{entered.resolve();return storage.promise;});
  await entered.promise;
  const closing=f.owner.disconnect(()=>assert.fail("expired opening must not flush"));
  const rejected=assert.rejects(closing,/Authentication before close/);
  t.mock.timers.tick(30_000);await rejected;await f.owner.settled();
  storage.resolve({drafts:{late:true}});await authentication;
  await f.transport.onmessage({data:{kind:"view",view:{frame_id:1},assets:{}}});
  assert.equal(f.owner.phase,"Failed");assert.equal(f.owner.drafts,undefined);assert.equal(frames,0);
  assert.equal(f.calls.filter(call=>call.method==="disconnect").length,0);
});

test("disconnect receipt deadline fails closed and rejects pending admission",async t=>{
  t.mock.timers.enable({apis:["setTimeout"]});
  const receipt=gate(),f=await fixture(),ordinary=f.owner.request("command","{}");
  const ordinaryRejected=assert.rejects(ordinary,/Disconnect receipt/);
  const post=f.transport.postMessage.bind(f.transport);
  let late;
  f.transport.postMessage=message=>{
    if(message.method==="disconnect"){late=message;receipt.resolve();}
    else post(message);
  };
  const closing=f.owner.disconnect(async()=>{});
  const rejected=assert.rejects(closing,/Disconnect receipt/);
  await receipt.promise;t.mock.timers.tick(30_000);await rejected;await ordinaryRejected;
  await f.owner.settled();
  f.transport.onmessage({data:{kind:"reply",id:late.id,result:{active_requests:0}}});
  assert.equal(f.owner.phase,"Failed");assert.equal(f.transport.terminated,1);
  assert.equal(f.failures.length,1);
});


test("lane capacity releases once for replies and rejects, including duplicate replies and send failure", async () => {
  const f = await fixture();
  const submit = (method, payload) => f.owner.request(method, payload).catch(error => error);
  const pending = Array.from({length: 8}, () => submit("command", "{}"));
  const first = f.calls.at(-8);
  const terminal = submit("terminal", '{"request":{"type":"read"}}');
  assert.equal((await submit("command", "{}")).notAdmitted, true);
  await f.transport.onmessage({data: {kind: "reply", id: first.id, error: "read failed"}});
  assert.match((await pending[0]).message, /read failed/);
  const replacement = submit("command", "{}");
  const admitted = f.calls.length;
  await f.transport.onmessage({data: {kind: "reply", id: first.id, result: "duplicate"}});
  await f.transport.onmessage({data: {kind: "reply", id: -1, result: "unknown"}});
  assert.equal((await submit("command", "{}")).notAdmitted, true);
  assert.equal(f.calls.length, admitted);
  const second = f.calls.find(call => call.method === "command" && call.id !== first.id);
  await f.transport.onmessage({data: {kind: "reply", id: second.id, result: "ok"}});
  const post = f.transport.postMessage;
  f.transport.postMessage = () => { throw new Error("send failed"); };
  assert.match((await submit("command", "{}")).message, /send failed/);
  f.transport.postMessage = post;
  const afterFailure = submit("command", "{}");
  assert.equal(f.calls.length, admitted + 1);
  await f.owner.retire();
  const settled = await Promise.all([...pending, terminal, replacement, afterFailure]);
  assert.ok(settled.slice(2).every(error => /replaced/.test(error.message)));
});
