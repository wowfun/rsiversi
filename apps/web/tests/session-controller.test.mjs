import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {mount,unmount} from '../src/runtime/session-elements.js';
import {upload} from '../src/runtime/session-submissions.js';

test('image import reports the pending files before Media replies and clears a failed batch',async()=>{
  const files=['one.png','two.png'].map(name=>new File(['png'],name,{type:'image/png'}));
  const editor={images:[],text:'draft',edit(_text,images){this.images=images;},async flush(){}};
  const pane={index:'main',generation:'1',editor,renderComposer(){}};
  let finishFirst, enterFirst, enterSecond;
  const first=new Promise(resolve=>{enterFirst=resolve;});
  const second=new Promise(resolve=>{enterSecond=resolve;});
  let failSecond;
  const context={connection:{},view:{media_limits:{images:8,upload_bytes:1024}},call(){
    if(!finishFirst)return new Promise(resolve=>{finishFirst=resolve;enterFirst();});
    enterSecond();return new Promise((_resolve,reject)=>{failSecond=reject;});
  }};
  const importing=upload(pane,context,files);
  assert.equal(pane.uploading.files,files);
  assert.equal(pane.uploading.next,0);
  await first;
  finishFirst(JSON.stringify({width:20,height:30}));
  await second;
  assert.deepEqual(editor.images,[{width:20,height:30}]);
  assert.equal(pane.uploading.next,1);
  failSecond(Error('media refused second file'));
  await assert.rejects(importing,/media refused/);
  assert.equal(pane.uploading,undefined);
  assert.equal(editor.images.length,1);
});

test('visibility starts empty and clears a displayed card once when its renderer retires',async()=>{
  const calls=[];
  const pane=new(createPaneClass({connection:{request:async(_,body)=>calls.push(JSON.parse(body))},notify:assert.fail}))('main');
  Object.assign(pane,{generation:'1',node:{isConnected:true},uiCards:false,visibleBlocks:{ready:true,last:()=>[['card',{sourceKey:'source'}]]}});
  pane.syncVisible();pane.scheduleVisible();
  assert.deepEqual(calls,[]);
  pane.uiCards=true;pane.syncVisible();
  pane.uiCards=false;pane.syncVisible();
  while(pane.visibleRunning)await new Promise(resolve=>setTimeout(resolve,1));
  pane.syncVisible();pane.scheduleVisible();
  assert.deepEqual(calls.map(call=>call.keys),[['card'],[]]);
});

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
  const originalWindow=globalThis.window;globalThis.window=new Node();
  globalThis.ResizeObserver=class{observe(){}disconnect(){}};
  t.after(()=>{if(original===undefined)delete globalThis.ResizeObserver;else globalThis.ResizeObserver=original;});
  t.after(()=>{if(originalWindow===undefined)delete globalThis.window;else globalThis.window=originalWindow;});
  const elements=Object.fromEntries(['node','history','live','commands','transcript','bottom','input','composer','modelReceipt','imageInput','cancel','steer','send','protectedExport'].map(key=>[key,new Node()]));
  let rendered=0;
  const pane={index:'main',renderComposer(){rendered++;},scheduleVisible(){},setReadingSummary(value){this.readingSummary=value;}};
  mount(pane,{TranscriptVisibility:class{dispose(){}}},elements);
  window.listeners['rsi:draft-storage-retired']();assert.equal(rendered,1);
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

test('manual forks flush the editor and refuse concurrent or replaced pane authority', async () => {
  const calls=[];
  const pane=new(createPaneClass({command:async value=>calls.push(value)}))('main');
  pane.generation='1';pane.renderComposer=()=>{};
  let release;
  pane.flush=()=>new Promise(resolve=>{release=resolve});
  const pending=pane.action('fork_conversation',{turn:'completed',turns:{last:2}});
  assert.equal(pane.switching,true);
  await assert.rejects(pane.action('fork_conversation',{turn:'another',turns:'all'}),/current operation/);
  assert.equal(calls.length,0);
  release();await pending;
  assert.deepEqual(calls,[{action:'fork_conversation',pane:'main',generation:'1',turn:'completed',turns:{last:2}}]);
  assert.equal(pane.switching,false);
  const stale=pane.action('fork_conversation',{turn:'completed',turns:'all'});
  pane.generation='2';release();
  await assert.rejects(stale,/Conversation changed/);
  assert.equal(calls.length,1);
});

test('document resource reads reject late attachment and connection replacements', async () => {
  for (const replacement of ['attachment', 'connection', 'closing']) {
    let finish;
    const context = {connection: {}, call: async (method, body) => {
      assert.equal(method, 'read_session_resource');
      assert.equal(JSON.parse(body).generation, '1');
      return new Promise(resolve => { finish = resolve; });
    }};
    const pane = new (createPaneClass(context))('main');
    pane.generation = '1';
    const pending = pane.action('document_resource', {request: {kind: 'sources'}});
    if (replacement === 'attachment') pane.generation = '2';
    if (replacement === 'connection') context.connection = {};
    if (replacement === 'closing') context.connection.closing = true;
    finish('{}');
    await assert.rejects(pending, /Resource Session was replaced/);
  }
});
