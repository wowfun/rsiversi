import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {mount,unmount} from '../src/runtime/session-elements.js';

test('failed visible delivery can retry unchanged membership with a fresh sequence',async()=>{
  const calls=[],notices=[];let refuse=true;
  const pane=new(createPaneClass({connection:{request:async(_,body)=>{calls.push(JSON.parse(body));if(refuse)throw Error('capacity');}},notify:message=>notices.push(message)}))('main');
  Object.assign(pane,{generation:'1',node:{isConnected:true},uiCards:true,visibleBlocks:{ready:true,last:()=>[['card',{sourceKey:'source'}]]}});
  const settled=async()=>{while(pane.visibleRunning)await new Promise(resolve=>setTimeout(resolve,5));};
  pane.syncVisible();await settled();assert.equal(calls.length,2);assert.equal(calls[0].sequence,calls[1].sequence);
  refuse=false;pane.syncVisible();await settled();assert.equal(calls.length,3);assert.notEqual(calls[2].sequence,calls[0].sequence);
  pane.syncVisible();await settled();assert.equal(calls.length,3);assert.equal(notices.length,1);
});

test('transcript End navigation respects nested editing, selection and handled events',t=>{
  class Node{listeners={};addEventListener(name,handler){this.listeners[name]=handler;}}
  const original=globalThis.ResizeObserver;
  globalThis.ResizeObserver=class{observe(){}disconnect(){}};
  t.after(()=>{if(original===undefined)delete globalThis.ResizeObserver;else globalThis.ResizeObserver=original;});
  const elements=Object.fromEntries(['node','history','live','commands','transcript','bottom','input','composer','modelReceipt','imageInput','attach','addReference','exportButton','searchHistory','addFile','cancel','steer','send','protectedExport'].map(key=>[key,new Node()]));
  const pane={index:'main',scheduleVisible(){},setReadingSummary(value){this.readingSummary=value;}};
  mount(pane,{TranscriptVisibility:class{dispose(){}}},elements);
  const transcript=pane.transcript;transcript.scrollHeight=1000;
  for(const extra of [{target:new Node()},{defaultPrevented:true},{shiftKey:true},{isComposing:true}]){
    pane.readingSummary=true;transcript.scrollTop=20;
    transcript.listeners.keydown({key:'End',target:transcript,preventDefault(){},...extra});
    assert.equal(transcript.scrollTop,20);assert.equal(pane.readingSummary,true);
  }
  let prevented=false;transcript.listeners.keydown({key:'End',target:transcript,preventDefault(){prevented=true;}});
  assert.equal(transcript.scrollTop,1000);assert.equal(pane.readingSummary,false);assert(prevented);
  unmount(pane,{});
});

test('an older visibility failure cannot invalidate newer membership or generation',async()=>{
  for(const generation of ['1','2']){
    const calls=[];let rejectOld,secondEntered;
    const second=new Promise(resolve=>{secondEntered=resolve;});
    const pane=new(createPaneClass({connection:{request:async(_,body)=>{
      calls.push(JSON.parse(body));
      if(calls.length===1)throw Error('retry');
      if(calls.length===2){secondEntered();await new Promise((_,reject)=>{rejectOld=reject;});}
    }},notify(){}}))('main');
    let entries=[['old',{sourceKey:'old'}]];
    Object.assign(pane,{generation:'1',node:{isConnected:true},uiCards:true,visibleBlocks:{ready:true,last:()=>entries}});
    pane.syncVisible();await second;
    entries=[['new',{sourceKey:'new'}]];pane.generation=generation;pane.syncVisible();rejectOld(Error('old refusal'));
    while(pane.visibleRunning)await new Promise(resolve=>setTimeout(resolve,5));
    assert.deepEqual(calls.map(call=>call.keys),[['old'],['old'],['new']]);
    assert.equal(calls[2].generation,generation);
    pane.syncVisible();assert.equal(calls.length,3);
  }
});

test('staged but never mounted panes can be cleared after a failed React commit', async () => {
  const Pane = createPaneClass({notify: assert.fail});
  const pane = new Pane('main');
  pane.staged = {data: {generation: '1'}};
  pane.reset();
  assert.equal(pane.staged, undefined);
  assert.equal(pane.composerView.disabled, true);
  // Also cover a previously committed generation whose host has been detached.
  pane.generation = '1';
  pane.reset();
  await pane.binding;
  assert.equal(pane.generation, undefined);
});

test('pane disposal releases the mount scope and clears staged authority', () => {
  const pane = new (createPaneClass({notify: assert.fail}))('main');
  pane.events = new AbortController();
  let disconnected = 0, disposed = 0;
  pane.resizeObserver = {disconnect() {disconnected++}};
  pane.visibleBlocks = {dispose() {disposed++}};
  pane.staged = {data: {generation: '1'}};
  pane.dispose();
  assert(pane.events.signal.aborted);
  assert.equal(disconnected, 1);
  assert.equal(disposed, 1);
  assert.equal(pane.staged, undefined);
});
