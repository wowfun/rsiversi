import test from 'node:test';
import assert from 'node:assert/strict';
import {submit,executePending} from '../src/runtime/session-submissions.js';

function fixture() {
  const events=[], connection={closing:false}, saved={revision:1};
  const editor={record:saved,text:'input',images:[],references:[],
    async update(change){this.record=await change(this.record)},
    store:{
      async freeze(record,prepared){assert.equal(record,saved);events.push('freeze');return {...record,pending:prepared}},
      async begin(record){events.push('begin');return {...record,pending:{...record.pending,phase:'dispatching'}}},
      async settle(record,result){assert.equal(record,editor.record);events.push(result.status);return {...record,result}},
    }};
  const context={connection,notify:assert.fail,async call(method){events.push(method);return JSON.stringify(method==='prepare_submission'?{opaque:'prepared',kind:'message',phase:'prepared'}:{status:'complete'})}};
  const pane={index:'main',generation:'1',editor,composerView:{access:'workspace-write'},composerActions:{revision:'7',primary:{id:'send'}},renderComposer(){},async flush(){events.push('flush')},executePending:(...args)=>executePending(pane,context,...args)};
  return {pane,context,editor,events,connection};
}

test('submission persists prepared and dispatching states before sending and settles the exact record',async()=>{
  const {pane,context,events}=fixture();
  await submit(pane,context);
  assert.deepEqual(events,['flush','prepare_submission','freeze','begin','dispatch_submission','complete']);
  assert.equal(pane.submitting,false);
});

test('failed dispatch persistence never sends a prepared submission',async()=>{
  const {pane,context,editor,events}=fixture();
  editor.store.begin=async()=>{throw Error('disk full')};
  await assert.rejects(submit(pane,context),/disk full/);
  assert.deepEqual(events,['flush','prepare_submission','freeze']);
  assert.equal(editor.record.pending.phase,'prepared');
});

for(const mode of ['dispatch','retry_message','query'])test(`retirement before ${mode} preserves the correct admission knowledge`,async()=>{
  const {pane,context,editor,events,connection}=fixture();
  editor.record={pending:{opaque:'saved',phase:mode==='dispatch'?'prepared':'dispatching'}};
  const begin=editor.store.begin;
  editor.store.begin=async record=>{const result=await begin(record);connection.closing=true;return result};
  if(mode!=='dispatch')context.connection={};
  await assert.rejects(executePending(pane,context,editor,'1',connection,mode),/Connection changed before dispatch/);
  assert.deepEqual(events,mode==='dispatch'?['begin','not_admitted']:['unknown']);
});
