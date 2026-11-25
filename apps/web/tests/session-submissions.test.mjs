import 'fake-indexeddb/auto';
import {DraftStore,DraftEditor} from '../drafts.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import {submit,reconcile,executePending,executeDocumentCommand} from '../src/runtime/session-submissions.js';

function fixture() {
  const events=[], connection={closing:false}, saved={revision:1,text:"input",images:[],references:[]}, referenceState={};
  const editor={record:saved,text:'input',images:[],references:[],
    get savedInput(){return this.record},
    async update(change){this.record=await change(this.record)},
    async settle(record,result){assert.equal(record,this.record);events.push(result.status);this.record={...record,result}},
    store:{
      async startDispatch(record,prepared){assert.equal(record,saved);events.push('claim');return {...record,pending:{...prepared,phase:'dispatching'}}},
      async claimPrepared(record){events.push('claim saved');return {...record,pending:{...record.pending,phase:'dispatching'}}},
    }};
  const context={connection,notify:assert.fail,async call(method){events.push(method);return JSON.stringify(method==='prepare_submission'?{opaque:'prepared',kind:'message',phase:'prepared'}:{status:'complete'})}};
  const pane={index:'main',generation:'1',editor,referenceState:()=>referenceState,composerView:{access:'workspace-write'},composerActions:{revision:'7',primary:{id:'send'}},renderComposer(){},async drainReferenceReleases(){},async flush(){events.push('flush')},executePending:(...args)=>executePending(pane,context,...args)};
  return {pane,context,editor,events,connection};
}

test('submission commits one dispatch claim before sending and settles the exact record',async()=>{
  const {pane,context,events}=fixture();
  await submit(pane,context);
  assert.deepEqual(events,['flush','prepare_submission','claim','dispatch_submission','complete']);
  assert.equal(pane.submitting,undefined);
});

test('failed dispatch persistence never sends a prepared submission',async()=>{
  const {pane,context,editor,events}=fixture();
  editor.store.startDispatch=async()=>{throw Error('disk full')};
  await assert.rejects(submit(pane,context),/disk full/);
  assert.deepEqual(events,['flush','prepare_submission']);
  assert.equal(editor.record.pending,undefined);
  assert.equal(editor.text,'input');
});

test('reopening with an old device Full access choice sends under the current Session policy',async()=>{
  const {pane,context}=fixture();
  pane.accessPreference={mode:'danger-full-access'};
  pane.composerView.access='read-only';
  const call=context.call;
  context.call=async(method,payload)=>{
    if(method==='prepare_submission')assert.equal(Object.hasOwn(JSON.parse(payload),'sandbox'),false);
    return call(method,payload);
  };
  await submit(pane,context);
});

for(const mode of ['dispatch','retry_message','query'])test(`retirement before ${mode} preserves the correct admission knowledge`,async()=>{
  const {pane,context,editor,events,connection}=fixture();
  editor.record={pending:{opaque:'saved',phase:mode==='dispatch'?'prepared':'dispatching'}};
  editor.flush=async()=>{if(mode!=='dispatch')context.connection={};};
  const claim=editor.store.claimPrepared;
  editor.store.claimPrepared=async record=>{const result=await claim(record);connection.closing=true;return result};
  await assert.rejects(reconcile(pane,context,mode),/Connection changed before dispatch/);
  assert.deepEqual(events,mode==='dispatch'?['claim saved','not_admitted']:['unknown']);
});

for (const mode of ['complete','unknown','missing','replaced']) test(`document command ${mode} preserves the editor and exact submission ownership`, async () => {
  const {pane,context,editor,events} = fixture();
  editor.flush = pane.flush;
  editor.store.startDispatch = async (record, prepared, preserve) => {
    assert.equal(preserve, true); events.push('claim'); return {...record, pending: {...prepared, phase:'dispatching'}};
  };
  context.call = async (method, payload) => {
    events.push(method);
    if (method === 'prepare_submission') {
      assert.equal(JSON.parse(payload).text, '/plan off');
      if (mode === 'replaced') pane.generation = '2';
      return JSON.stringify({kind: mode === 'missing' ? 'message' : 'command', opaque: 'exact-command', phase: 'prepared'});
    }
    assert.equal(payload.opaque, 'exact-command');
    return JSON.stringify({status: mode});
  };
  const operation = executeDocumentCommand(pane,context,'plan','off');
  assert.equal(pane.submitting, 'Running command');
  if (mode === 'complete') await operation;
  else await assert.rejects(operation, mode === 'missing' ? /unavailable/ : mode === 'replaced' ? /changed/ : /confirmation/);
  assert.equal(editor.text, 'input');
  assert.equal(pane.submitting, undefined);
  assert.equal(events.includes('dispatch_submission'), ['complete','unknown'].includes(mode));
});


test('typing while preparation is pending survives completed submission in the real draft store',async()=>{
  const store=await DraftStore.open('a'.repeat(32),{kind:'local'});
  const editor=new DraftEditor(store,await store.ensure('main','session','b'.repeat(64)));
  editor.edit('submitted text');await editor.flush();
  const {pane,context}=fixture();pane.editor=editor;pane.flush=()=>editor.flush();
  let entered,release;
  const preparing=new Promise(resolve=>entered=resolve),gate=new Promise(resolve=>release=resolve);
  context.call=async(method,payload)=>{
    if(method==='prepare_submission'){
      assert.equal(JSON.parse(payload).text,'submitted text');entered();await gate;
      return JSON.stringify({kind:'message',id:'saved-request',opaque:'prepared',text_bytes:14,images:0,captures:[]});
    }
    return JSON.stringify({status:'complete',receipt:'acknowledged'});
  };
  const sending=submit(pane,context);
  await preparing;editor.edit('typed during preparation');await editor.flush();
  release();await sending;
  assert.equal(editor.text,'typed during preparation');
  assert.equal((await store.get('main','session')).text,'typed during preparation');
  assert.equal(editor.record.pending,null);
  await store.verify();
});
