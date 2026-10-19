import assert from 'node:assert/strict';
import {test} from 'node:test';
import {externalDrafts} from '../external-drafts.js';

test('byte pressure retains zero-weight records and unbinding refreshes idle recency',()=>{
  const store=externalDrafts(), changed=()=>{};
  const empty=store.bind('empty',changed);store.unbind(empty,changed);
  const full=store.bind('full',changed);store.edit(full,'x'.repeat(1024*1024));store.unbind(full,changed);
  const next=store.bind('next',changed);assert.equal(store.edit(next,'x'),true);
  assert.equal(store.current(empty),true);assert.equal(store.current(full),false);
  store.clear();
  const active=store.bind('active',changed), idle=[];
  for(let i=0;i<15;i++){const record=store.bind(String(i),changed);store.unbind(record,changed);idle.push(record);}
  store.unbind(active,changed);store.bind('new',changed);
  assert.equal(store.current(active),true);assert.equal(store.current(idle[0]),false);
});

test('draft capacity evicts idle uncertainty with text, pins bindings and pending bytes',()=>{
  const store=externalDrafts(), changed=()=>{};
  const first=store.bind('first',changed);store.edit(first,'unknown');store.finish(store.begin(first),false);store.unbind(first,changed);
  for(let i=0;i<16;i++){const record=store.bind(String(i),changed);assert(record);}
  assert.equal(store.current(first),false);assert.equal(store.bind('overflow',changed),null);
  store.clear();const reopened=store.bind('first',changed);assert.equal(reopened.text,'');assert.equal(reopened.unknownRevision,null);
  store.edit(reopened,'é'.repeat(256*1024));const pending=store.begin(reopened);
  assert.equal(store.edit(reopened,'x'.repeat(512*1024)),true);
  assert.equal(store.edit(reopened,'x'.repeat(512*1024+1)),false);
  assert.equal(reopened.text.length,512*1024);
  store.unbind(reopened,changed);
  const next=store.bind('next',changed);assert.equal(store.edit(next,'x'),false);
  store.finish(pending,false);assert.equal(store.edit(next,'x'),true);
  assert.equal(reopened.unknownRevision,null,'old revision failure does not fence an edited draft');
});

test('draft admission measures only changed text, including pending revisions',t=>{
  const store=externalDrafts(), changed=()=>{}, records=[];
  for(let i=0;i<16;i++) {
    const record=store.bind(String(i),changed);
    assert.equal(store.edit(record,'é'.repeat(32*1024)),true);
    records.push(record);
  }
  const encoded=[], encode=TextEncoder.prototype.encode;
  t.mock.method(TextEncoder.prototype,'encode',function(text) {
    const bytes=encode.call(this,text);
    encoded.push(bytes.length);
    return bytes;
  });
  const record=records[0];
  assert.equal(store.edit(record,'短'),true);
  const pending=store.begin(record);
  assert.equal(store.edit(record,'新'),true);
  assert.equal(store.edit(record,'新'),true);
  store.finish(pending,true);
  assert.equal(record.text,'新','old completion preserves the newer revision');
  store.finish(store.begin(record),true);
  assert.equal(record.text,'');
  assert.equal(store.edit(record,'é'.repeat(32*1024)),true,'success releases cached byte weight');
  assert.equal(store.edit(records[1],records[1].text+'x'),false,'UTF-8 bytes still enforce the exact cap');
  assert.deepEqual(encoded,[3,3,64*1024,64*1024+1]);
});

test('clear fences old records before callbacks and preserves a new connection attempt',()=>{
  const store=externalDrafts();
  let clearing=false, previous, oldAttempt, fresh, freshAttempt, notifications=0;
  const changed=()=>{
    if(!clearing) return;
    notifications++;
    assert.equal(store.current(previous),false);
    assert.equal(previous.text,'');
    assert.equal(store.begin(previous),null);
    store.finish(oldAttempt,true);
    fresh=store.bind('same-conversation',()=>{});
    assert.notEqual(fresh,previous);
    assert.equal(store.edit(fresh,'new connection'),true);
    freshAttempt=store.begin(fresh);
    assert(freshAttempt);
  };
  previous=store.bind('same-conversation',changed);
  store.edit(previous,'old connection');
  oldAttempt=store.begin(previous);
  clearing=true;
  store.clear();
  assert.equal(notifications,1);
  assert.equal(previous.bindings.size,0);
  store.finish(oldAttempt,false);
  assert.equal(fresh.text,'new connection');
  assert.equal(fresh.unknownRevision,null);
  assert.equal(fresh.pending,freshAttempt);
  store.finish(freshAttempt,true);
  assert.equal(fresh.text,'');
  assert.equal(fresh.pending,null);
});
