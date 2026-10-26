import test from 'node:test';
import assert from 'node:assert/strict';
import {ResourceOwner} from '../src/resource-owner.mjs';
const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no});return {promise,resolve,reject}};
const tick=()=>new Promise(resolve=>setImmediate(resolve));
const entry=(view='1',open_revision='2')=>({view,open_revision});

test('exact cleanup retries a busy refusal but respects a permanent refusal',async()=>{
  for(const permanent of [false,true]){
    let calls=0;const error=Object.assign(new Error('Input is busy'),{notAdmitted:true,...(permanent?{retryable:false}:{})});
    const owner=new ResourceOwner(async()=>{if(++calls===1)throw error});
    owner.reservePanel('s',entry()).commit();await tick();
    assert.equal(owner.getSnapshot()[0].retryable,!permanent);
    owner.retry('panel:1:2');await tick();assert.equal(calls,permanent?1:2);
  }
});

test('unknown allocation is actionable without replaying a rejected promise',async()=>{
  let calls=0;const owner=new ResourceOwner(()=>{calls++;return Promise.reject(new Error('lost reply'))});
  const created=owner.attachment('session','main','old',{type:'create'});
  await assert.rejects(created.promise);await tick();
  const notice=owner.getSnapshot()[0];assert.equal(notice.retryable,false);
  owner.retry(notice.key);await tick();assert.equal(calls,1);
});
test('malformed open receipts reconcile from the matching frame and then clean up',async()=>{
  let calls=0;const owner=new ResourceOwner(()=>++calls===1?Promise.resolve('{'):Promise.resolve(),()=> 'request');
  const open=owner.open('session',{});await assert.rejects(open.promise);await tick();
  assert.equal(owner.getSnapshot()[0].retryable,false);
  owner.reserveOpen(open).commit();await tick();
  owner.observe([{...entry(),opening_request:'request'}]);await tick();
  assert.equal(calls,2);owner.observe([]);
  assert.equal(owner.opens.size,0);assert.equal(owner.cleanup.size,0);
});
test('known terminal refusal releases even when closure preceded the reply',async()=>{
  const pending=deferred(),owner=new ResourceOwner(()=>pending.promise);
  const created=owner.attachment('s','main','1',{type:'attach',terminal:'stale'});created.close();
  pending.reject(Object.assign(new Error('unavailable'),{notAdmitted:true}));
  await assert.rejects(created.promise);await tick();
  assert.equal(owner.cleanup.size,0);assert.deepEqual(owner.getSnapshot(),[]);
});

test('matching frames reconcile an unknown opening across cleanup settlement',async()=>{
  for(let delay=0;delay<8;delay++){
    let calls=0;const owner=new ResourceOwner(()=>++calls===1?Promise.resolve('{'):Promise.resolve(),()=> 'request');
    const open=owner.open('session',{});await assert.rejects(open.promise);
    owner.reserveOpen(open).commit();
    for(let step=0;step<delay;step++)await Promise.resolve();
    owner.observe([{...entry(),opening_request:'request'}]);await tick();
    assert.equal(calls,2,`settlement delay ${delay}`);owner.observe([]);
    assert.equal(owner.cleanup.size,0);assert.equal(owner.opens.size,0);
  }
});

test('saving failure cancels reserved cleanup; network failure never changes committed layout',async()=>{
  const calls=[],owner=new ResourceOwner(async(...args)=>{calls.push(args);throw new Error('offline')});
  owner.reservePanel('session',entry()).cancel();await tick();assert.equal(calls.length,0);
  const close=owner.reservePanel('session',entry());let layout=['tab'];
  layout=[];close.commit();await tick();
  assert.deepEqual(layout,[]);assert.equal(calls.length,1);assert.match(owner.getSnapshot()[0].error,/offline/);
  assert.equal(JSON.parse(calls[0][1]).expected_open_revision,'2');
});
test('explicit retries deduplicate in-flight cleanup and retain the original connection',async()=>{
  const waiting=deferred();let calls=0;
  const owner=new ResourceOwner(()=>{calls++;return waiting.promise});
  owner.reservePanel('session',entry()).commit();await tick();
  owner.retry('panel:1:2');owner.retry('panel:1:2');await tick();assert.equal(calls,1);
  waiting.reject(new Error('refused'));await tick();assert.equal(owner.cleanup.size,1);
  owner.retire();owner.retry('panel:1:2');assert.equal(calls,1);
});
test('pending close consumes the exact late receipt without requiring a frame',async()=>{
  const waiting=deferred(),calls=[];
  const owner=new ResourceOwner((method,text)=>{calls.push([method,JSON.parse(text)]);return method==='resource_open'?waiting.promise:Promise.resolve()},()=> 'request-a');
  const open=owner.open('session',{action:'inspect_block'});
  owner.reserveOpen(open).commit();await tick();
  waiting.resolve(JSON.stringify({status:'opened',request:'request-a',...entry()}));await tick();
  assert.equal(calls[1][0],'command');assert.equal(calls[1][1].view,'1');
  assert.equal(owner.suppresses({...entry(),opening_request:'request-a'}),true);
  owner.observe([{...entry(),opening_request:'request-a'}]);assert.equal(owner.cleanup.size,1);
  owner.observe([]);assert.equal(owner.cleanup.size,0);assert.equal(owner.opens.size,0);
});
test('independent same-coordinate opens can complete in reverse order',async()=>{
  const pending=[deferred(),deferred()],calls=[];let mint=0,read=0;
  const owner=new ResourceOwner((method,text)=>method==='resource_open'?pending[read++].promise:(calls.push(JSON.parse(text)),Promise.resolve()),()=>`request-${++mint}`);
  const a=owner.open('s',{action:'inspect_interaction'}),b=owner.open('s',{action:'inspect_interaction'});
  owner.reserveOpen(a).commit();await tick();
  pending[1].resolve(JSON.stringify({status:'opened',request:b.request,...entry('3','4')}));await tick();
  pending[0].resolve(JSON.stringify({status:'opened',request:a.request,...entry('1','2')}));await tick();
  assert.equal(calls.length,1);assert.equal(calls[0].view,'1');assert.equal(b.closed,false);
  assert.equal(owner.suppresses({...entry('3','4'),opening_request:b.request}),false);
});
test('Unavailable and known non-admission terminate pending cleanup',async()=>{
  for(const rejected of [false,true]){
    const owner=new ResourceOwner(()=>rejected?Promise.reject(Object.assign(new Error('busy'),{notAdmitted:true})):Promise.resolve(JSON.stringify({status:'unavailable',request:'r',reason:'settled'})),()=> 'r');
    const open=owner.open('s',{});owner.reserveOpen(open).commit();await tick();
    assert.equal(owner.opens.size,0);assert.equal(owner.cleanup.size,0);
  }
});
test('cleanup cannot exceed its session bound or discard failed responsibilities',async()=>{
  const owner=new ResourceOwner(()=>Promise.reject(new Error('offline')));
  for(let i=1;i<=16;i++)owner.reservePanel('s',entry(String(i))).commit();await tick();
  assert.throws(()=>owner.reservePanel('s',entry('17')),/capacity/);assert.equal(owner.cleanup.size,16);
});

test('terminal creation reserves cleanup before admission and late replies detach exactly once',async()=>{
  const gate=deferred(),calls=[];let next=0;
  const owner=new ResourceOwner((method,text)=>{
    const value=JSON.parse(text);calls.push(value);
    return value.request.type==='create'?gate.promise:Promise.resolve('{}');
  },()=>String(++next));
  const lease=owner.attachment('s','main','original',{type:'create'});
  for(let n=1;n<=15;n++)owner.reservePanel('s',entry(String(n))).commit();
  assert.throws(()=>owner.attachment('s','main','original',{type:'create'}),/capacity/);
  lease.close();lease.close();await tick();
  gate.resolve(JSON.stringify({value:{id:'late',terminal:{id:'shell'}}}));await tick();
  assert.equal(calls.filter(call=>call.request?.type==='create').length,1);
  assert.deepEqual(calls.filter(call=>call.request?.type==='detach'),[{pane:'main',generation:'original',request:{type:'detach',attachment:'late'}}]);
});

test('cleanup exposes a human title and preserves exact identity separately', async()=>{
  const owner=new ResourceOwner(()=>Promise.reject(new Error('Input is busy')));
  owner.reservePanel('opaque-session',entry(),'Workspace files').commit();await tick();
  assert.equal(owner.getSnapshot()[0].label,'Workspace files');
  assert.equal(owner.getSnapshot()[0].error,'Input is busy');
  assert.equal(owner.getSnapshot()[0].key,'panel:1:2');
});

test('an admitted open can always close at full cleanup capacity, without duplicate responsibility',async()=>{
  let sequence=0;const owner=new ResourceOwner(async(method,source)=>{
    const value=JSON.parse(source);
    return method==='resource_open'?JSON.stringify({status:'opened',request:value.opening_request,...entry(value.opening_request,'2')}):undefined;
  },()=>String(++sequence));
  const opens=[];
  for(let i=0;i<16;i++)opens.push(owner.open('s',{}));
  await Promise.all(opens.map(open=>open.promise));
  assert.equal(owner.cleanup.size,16);
  assert.throws(()=>owner.open('s',{}),/close an open resource/);
  const close=owner.reservePanel('s',entry('1','2'));close.cancel();
  assert.equal(owner.cleanup.size,16);
  owner.reserveOpen(opens[0]).commit();await tick();owner.observe([]);
  assert.equal(owner.opens.size,15);assert.equal(owner.cleanup.size,15);
  await owner.open('s',{}).promise;
});
test('unknown opens keep one bounded responsibility and one notice without RPC replay',async()=>{
  let sequence=0,calls=0;const owner=new ResourceOwner(async()=>{calls++;throw Error('unknown outcome')},()=>String(++sequence));
  for(let i=0;i<16;i++){
    const open=owner.open('s',{});await assert.rejects(open.promise);owner.reserveOpen(open).commit();await tick();
  }
  assert.equal(owner.cleanup.size,16);assert.equal(owner.getSnapshot().length,16);
  assert.equal(new Set(owner.getSnapshot().map(item=>item.key)).size,16);
  assert.throws(()=>owner.open('s',{}),/capacity/);assert.equal(calls,16);
});
test('retirement preserves the specific cleanup failure',async()=>{
  const owner=new ResourceOwner(async()=>{throw Error('specific refusal')});
  owner.reservePanel('s',entry()).commit();await tick();owner.retire(new Error('transport deadline'));
  assert.match(owner.getSnapshot()[0].error,/specific refusal.*transport deadline/);
});

test('a frame arriving before the open reply cannot activate an uncommitted cleanup reservation',async()=>{
  const pending=deferred();let calls=0;
  const owner=new ResourceOwner(method=>{calls++;return method==='resource_open'?pending.promise:Promise.resolve()},()=> 'request');
  const open=owner.open('s',{});await tick();
  owner.observe([{...entry(),opening_request:'request'}]);await tick();
  assert.equal(calls,1,'observing a live allocation is not permission to close it');
  pending.resolve(JSON.stringify({status:'opened',request:'request',...entry()}));await open.promise;
  owner.reserveOpen(open).commit();await tick();assert.equal(calls,2);
  owner.observe([]);assert.equal(owner.cleanup.size,0);
});

test('retirement refuses new resource allocation before dispatch',async()=>{
  let calls=0;const owner=new ResourceOwner(async()=>{calls++;return '{}'});
  owner.retire();
  assert.throws(()=>owner.open('s',{}),/retired/);
  await tick();assert.equal(calls,0);assert.equal(owner.opens.size,0);assert.equal(owner.cleanup.size,0);
});

test('a receipt whose original pane has retired releases its unobserved reservation', async()=>{
  let resolve;
  const owner=new ResourceOwner(()=>new Promise(done=>{resolve=done}),()=> 'late');
  const open=owner.open('session',{pane:'main',generation:'1'});
  await Promise.resolve();
  owner.observe([],{main:{generation:'2'}});
  resolve(JSON.stringify({status:'opened',request:'late',view:'1',open_revision:'1'}));
  await open.promise;
  assert.equal(owner.cleanup.size,0);assert.equal(owner.opens.size,0);
  owner.reserveOpen(open).commit();
  assert.equal(owner.cleanup.size,0);
});

test('cancelled cleanup never dispatches even if an old tab later commits it',async()=>{
  let calls=0;const owner=new ResourceOwner(async()=>{calls++});
  const reservation=owner.reservePanel('session',{view:'1',open_revision:'1'});
  reservation.cancel();reservation.commit();await Promise.resolve();
  assert.equal(calls,0);assert.equal(owner.cleanup.size,0);
});
