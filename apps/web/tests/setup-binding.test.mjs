import test from 'node:test';
import assert from 'node:assert/strict';
import {createSetupBinding, providerApplied} from '../src/runtime/setup-binding.ts';

function fixture() {
  let owner = {closing:false}, setup;
  const listeners = new Set(), calls = [];
  const binding = createSetupBinding({owner:()=>owner,closing:o=>o.closing,snapshot:()=>setup,
    subscribe:fn=>{listeners.add(fn);return()=>listeners.delete(fn)},
    command:async(o,c)=>{calls.push([o,c])}});
  return {binding,calls,listeners,get owner(){return owner}, replace(){owner={closing:false}},
    publish(next){setup=next;for(const read of listeners)read()}};
}
test('retirement clears waiters and prevents a continuation reaching a replacement owner',async()=>{
  for(const reason of ['unmount','dismiss','replacement','closing']) {
    const f=fixture(), scope=f.binding.open(), original=f.owner;
    await scope.command({kind:'providers_replace'});
    const pending=scope.waitSetup(()=>true);
    const rejected=assert.rejects(pending,/closed/);
    if(reason==='unmount')scope.retire();
    if(reason==='dismiss')f.binding.retire();
    if(reason==='replacement')f.replace();
    if(reason==='closing')f.owner.closing=true;
    f.publish({ticket:'new'});
    await rejected;
    await assert.rejects(scope.command({kind:'default_model'}),/closed/);
    assert.equal(f.calls.length,1);assert.equal(f.calls[0][0],original);assert.equal(f.listeners.size,0);
  }
});
test('each mount has independent retirement; replacement must reach the exact terminal revision',async()=>{
  const f=fixture(), old=f.binding.open();old.retire();const scope=f.binding.open();
  const pending=scope.waitSetup(s=>providerApplied(s,'9007199254740994'));
  let done=false;pending.then(()=>{done=true});
  const status=(desired,applied,applying)=>({providers:{desired_revision:desired,applied_revision:applied,applying,diagnostic:null}});
  f.publish(status('9007199254740993','9007199254740993',false));await Promise.resolve();assert.equal(done,false);
  f.publish(status('9007199254740994','9007199254740993',true));await Promise.resolve();assert.equal(done,false);
  f.publish(status('9007199254740994','9007199254740994',false));await pending;
  await scope.command({kind:'default_model'});assert.equal(f.calls.length,1);assert.equal(f.listeners.size,0);
  assert.throws(()=>providerApplied(status('9007199254740995','9007199254740995',false),'9007199254740994'),/changed again/);
  scope.retire();f.binding.retire();await assert.rejects(f.binding.open().command({}),/closed/);
});
