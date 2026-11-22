import {clearDraftRecords} from './clear-draft-records.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import 'fake-indexeddb/auto';
import {DraftStore, DraftEditor} from '../drafts.js';
import {StartingDraft} from '../starting-draft.js';
globalThis.requestAnimationFrame = callback => queueMicrotask(callback);

const record={key:['endpoint','local','main','saved'],header:'header',text:'',images:[],references:[],releases:[],pending:null,creation:{},everDispatched:false};
function fixture(records=[record]) {
  const calls=[],scope={connection:{drafts:{namespace:['endpoint','local'],list:async()=>({rows:records})}},call:async(_method,payload)=>{calls.push(JSON.parse(payload));return '{"status":"expired"}'}};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  pane.flush=async()=>{};pane.renderComposer=()=>{};pane.focusInput=()=>{};
  return {pane,scope,calls};
}
test('healthy starting drafts remain recoverable when Session storage fails',async t=>{
  const {pane,scope}=fixture([]);
  scope.connection.drafts.list=async()=>{throw Error('Session storage refused');};
  t.mock.method(StartingDraft,'list',async()=>({rows:[{key:JSON.stringify(['endpoint','local','main']),text:'healthy input'}]}));
  await pane.restoreDrafts();
  assert.deepEqual(pane.recoveryView.rows.map(row=>row.text),['healthy input']);
  assert.match(pane.recoveryView.notice,/Session storage refused/);
});
test('recovery reports failures of both independent stores',async t=>{
  const {pane,scope}=fixture([]);
  scope.connection.drafts.list=async()=>{throw Error('Session failure');};
  t.mock.method(StartingDraft,'list',async()=>{throw Error('Starting failure');});
  await pane.restoreDrafts();
  assert.match(pane.recoveryView.notice,/Session failure/);
  assert.match(pane.recoveryView.notice,/Starting failure/);
});
test('recovery exposes global discard and confirms deletion of nonempty input without remote obligations',async(t)=>{
  t.mock.method(StartingDraft,'list',async()=>({rows:[]}));
  const saved={...record,text:'unused input'}, {pane,scope}=fixture([saved]);
  const removed=[];scope.connection.drafts.removeSaved=async value=>removed.push(value);
  await pane.restoreDrafts();const panel=pane.recoveryView,row=panel.rows[0];
  assert.equal(typeof panel.discard,'function');assert.equal(row.removable,true);
  await row.remove();assert.equal(row.deleteConfirmed,true);assert.deepEqual(removed,[]);
  row.cancelDelete();assert.equal(row.deleteConfirmed,false);
  await row.remove();await row.remove();assert.deepEqual(removed,[saved]);assert.equal(panel.rows.length,0);
  let discards=0;pane.discardSavedDrafts=async()=>discards++;
  await panel.discard();assert.equal(discards,1);assert.equal(pane.recoveryView,undefined);
});

test('recovery reports loading and read failure rather than an empty successful list',async()=>{
  const {pane,scope}=fixture([]);const read=Promise.withResolvers();
  scope.connection.drafts.list=()=>read.promise;
  const opening=pane.restoreDrafts();
  assert.equal(pane.recoveryView?.loading,true);
  read.reject(Error('storage refused'));await opening;
  assert.equal(pane.recoveryView.loading,false);assert.equal(pane.recoveryView.busy,false);
  assert.match(pane.recoveryView.notice,/storage refused/);
});

test('loading more starting drafts retains earlier rows and cancellable deletion state',async(t)=>{
  const {pane}=fixture([]),first={key:JSON.stringify(['endpoint','local','main']),text:'first'},second={key:JSON.stringify(['endpoint','local','compare']),text:'second'};
  const cursors=[],removed=[];
  t.mock.method(StartingDraft,'list',async(_namespace,after)=>{cursors.push(after);return after?{rows:[second]}:{rows:[first],next:first.key};});
  t.mock.method(StartingDraft,'remove',async row=>removed.push(row));
  await pane.restoreDrafts();const panel=pane.recoveryView,row=panel.rows[0];
  await row.remove();assert.equal(row.deleteConfirmed,true);
  await panel.more();
  assert.deepEqual(panel.rows.map(row=>row.text),['first','second']);
  assert.equal(panel.rows[0],row);assert.equal(row.deleteConfirmed,true);assert.equal(panel.more,undefined);
  row.cancelDelete();assert.equal(row.deleteConfirmed,false);assert.deepEqual(removed,[]);
  await row.remove();assert.deepEqual(removed,[]);
  await row.remove();assert.deepEqual(removed,[first]);assert.deepEqual(panel.rows.map(row=>row.text),['second']);
  assert.deepEqual(cursors,[undefined,first.key]);
});

test('paging Session drafts replaces the visible Session rows and preserves starting input',async t=>{
  const {pane,scope}=fixture();
  const second={...record,key:['endpoint','local','main','two'],text:'second'};
  const cursors=[];
  scope.connection.drafts.list=async(_pane,after)=>{
    cursors.push(after);
    return after ? {rows:[second]} : {rows:[record],next:record.key[3]};
  };
  t.mock.method(StartingDraft,'list',async()=>({rows:[{key:JSON.stringify(['endpoint','local','compare']),text:'starting'}]}));
  await pane.restoreDrafts();
  const panel=pane.recoveryView,starting=panel.rows[1];
  assert.equal(panel.previousSessions,undefined);
  await panel.nextSessions();
  assert.equal(panel.nextSessions,undefined);
  assert.deepEqual(panel.rows.map(row=>row.text),['second','starting']);
  assert.equal(panel.rows[1],starting);
  await panel.previousSessions();
  assert.deepEqual(cursors,[undefined,'saved',undefined]);
  assert.deepEqual(panel.rows.map(row=>row.text),['','starting']);
});

test('Session paging waits for the initial starting-draft read',async t=>{
  const {pane,scope}=fixture();
  const loading=Promise.withResolvers(), started=Promise.withResolvers();
  let reads=0;
  scope.connection.drafts.list=async()=>{reads++;return {rows:[record],next:'saved'};};
  t.mock.method(StartingDraft,'list',async()=>{started.resolve();return loading.promise;});
  const opening=pane.restoreDrafts();
  await started.promise;
  const panel=pane.recoveryView;
  assert.equal(panel.busy,true);
  await panel.nextSessions();
  assert.equal(reads,1);
  loading.resolve({rows:[]});await opening;
  assert.equal(panel.busy,false);
});

test('recovery uses known conversation titles while retaining unavailable identities and full coordinates',async(t)=>{
  const {pane,scope}=fixture([record,{...record,key:['endpoint','local','main','expired-id']}]);
  scope.view={surfaces:{main:{kind:'native',session:'saved',workspace:{key:'test',coordinates:{location:{kind:'local'},path:'/workspace'},registration:{status:'unregistered'}}}},catalog:{workspaces:[]},navigation:{entries:[{session:'saved',metadata:{title:'Research notes'}}]}};
  t.mock.method(StartingDraft,'list',async()=>({rows:[{key:JSON.stringify(['endpoint','local','compare']),text:'starting'}]}));
  await pane.restoreDrafts();
  assert.equal(pane.recoveryView.rows[0].summary,'Research notes');
  assert.equal(pane.recoveryView.rows[0].coordinates,JSON.stringify(record.key));
  assert.equal(pane.recoveryView.rows[1].summary,'expired-id');
  assert.equal(pane.recoveryView.rows[2].summary,'Starting draft · Compare · starting');
});

test('recovery omits clean attached input while retaining other surfaces and current unsaved conflicts',async(t)=>{
  const current={...record,text:'already attached'};
  const other={...record,key:['endpoint','local','main','other'],text:'recover this'};
  const {pane}=fixture([current,other]);pane.editor={record:current,text:current.text};
  pane.editors.set(JSON.stringify(current.key),pane.editor);
  t.mock.method(StartingDraft,'list',async()=>({rows:[{key:JSON.stringify(['endpoint','local','main']),text:'active start'},{key:JSON.stringify(['endpoint','local','compare']),text:'other start'}]}));
  pane.starting={key:JSON.stringify(['endpoint','local','main'])};
  await pane.restoreDrafts();
  assert.deepEqual(pane.recoveryView.rows.map(row=>row.text),['recover this','other start']);
  pane.editor.failure=Error('unsaved');pane.starting.failure=Error('unsaved');
  await pane.restoreDrafts();
  assert.deepEqual(pane.recoveryView.rows.map(row=>row.text),['already attached','recover this','active start','other start']);
});

test('a failed first save remains discoverable only in its current namespace',async(t)=>{
  const store=await DraftStore.open('c'.repeat(32),{kind:'local'});
  try {
    const blank=await store.ensure('main','unsaved','a'.repeat(64),{workspace_id:'b'.repeat(64),session_id:'unsaved',agent_preset_id:null});
    const editor=new DraftEditor(store,blank);
    t.mock.method(store,'edit',async()=>{throw Error('disk full')});
    editor.edit('valuable unsaved text');await assert.rejects(editor.flush(),/disk full/);
    assert.equal((await store.list('main')).rows.length,0);
    const {pane,scope}=fixture([]);scope.connection.drafts=store;
    pane.editors.set(JSON.stringify(blank.key),editor);
    pane.editors.set('foreign',{record:{...blank,key:['other','local','main','foreign']},dirty:true,text:'private'});
    await pane.restoreDrafts();
    assert.equal(pane.recoveryView.rows.length,1);
    assert.equal(pane.recoveryView.rows[0].unsaved.text,'valuable unsaved text');
  } finally {await clearDraftRecords()}
});

test('removing an empty draft admits only one concurrent action',async()=>{
  const {pane,scope}=fixture(); let finish, calls=0;
  scope.connection.drafts.removeEmpty=()=>{calls++;return new Promise(resolve=>finish=resolve)};
  await pane.restoreDrafts(); const panel=pane.recoveryView,row=panel.rows[0];
  const first=row.remove(); const second=row.remove();
  assert.equal(panel.busy,true); assert.equal(calls,1);
  finish(); await Promise.all([first,second]);
  assert.equal(panel.busy,false); assert.deepEqual(panel.rows,[]);
});

test('saved-input summaries preserve Unicode characters and fit one line',async()=>{
  const {pane}=fixture([
    {...record,text:'x'.repeat(79)+'🚀'+'tail'},
    {...record,key:['endpoint','local','main','second'],text:'first\r\nsecond\t third'},
    {...record,key:['endpoint','local','main','third'],text:'🚀'.repeat(79)+'x🚀'},
    {...record,key:['endpoint','local','main','fourth'],text:'🚀'.repeat(80)+'tail'},
  ]);
  await pane.restoreDrafts();
  assert.equal(pane.recoveryView.rows[0].summary,'saved · '+'x'.repeat(79)+'🚀');
  assert.equal(pane.recoveryView.rows[1].summary,'second · first second third');
  assert.equal(pane.recoveryView.rows[2].summary,'third · '+'🚀'.repeat(79)+'x');
  assert.equal(pane.recoveryView.rows[3].summary,'fourth · '+'🚀'.repeat(80));
});

test('expired Fresh recreation requires undispatched input without references',async()=>{
  const {pane}=fixture([{...record,references:[{snapshot:{capture_id:'retained'}}]}, {...record,key:['endpoint','local','main','dispatched'],everDispatched:true}, {...record,key:['endpoint','local','main','recreate'],text:'recreate me'}]);
  await pane.restoreDrafts();const [first,second,third]=pane.recoveryView.rows;
  assert.equal(first.empty,false);
  await first.open();assert.equal(first.expired,true);assert.equal(first.canRecreate,false);
  await second.open();assert.equal(second.expired,true);assert.equal(second.canRecreate,false);
  await third.open();assert.equal(third.expired,true);assert.equal(third.canRecreate,true);
});

test('closing recovery during draft flush fences its queued open request',async()=>{
  const {pane,calls}=fixture();let release;
  pane.flush=()=>new Promise(resolve=>release=resolve);
  await pane.restoreDrafts();const panel=pane.recoveryView,opening=panel.rows[0].open();
  panel.close();release();await opening;
  assert.equal(calls.length,0);assert.equal(pane.recoveryView,undefined);
});

test('closing or successfully opening saved drafts returns focus to the composer',async()=>{
  const {pane,scope}=fixture(); let focused=0;
  pane.focusInput=()=>{focused++;};
  await pane.restoreDrafts(); pane.recoveryView.close(); await Promise.resolve();
  assert.equal(focused,1);
  scope.call=async()=>'{"status":"opened"}';
  await pane.restoreDrafts(); await pane.recoveryView.rows[0].open();
  assert.equal(pane.recoveryView,undefined); assert.equal(focused,2);
});

test('a retired recovery reply cannot change the replacement connection switching state',async()=>{
  const {pane,scope}=fixture();let release;
  scope.call=()=>new Promise(resolve=>release=resolve);
  await pane.restoreDrafts();const panel=pane.recoveryView,opening=panel.rows[0].open();
  await Promise.resolve();await Promise.resolve();
  scope.connection={};pane.switching=true;release('{"status":"expired"}');await opening;
  assert.equal(pane.switching,true);assert.equal(panel.rows[0].expired,undefined);
});

test('a release-only record remains recoverable until cleanup is acknowledged', async()=>{
  const {pane}=fixture([{...record,releases:[{snapshot:{capture_id:'retiring'}}]}]);
  await pane.restoreDrafts();
  assert.equal(pane.recoveryView.rows[0].empty,false);
  assert.match(pane.recoveryView.rows[0].summary,/pending reference cleanup/);
});

test('expired Fresh recovery with releases offers cleanup before recreation',async()=>{
  const saved={...record,creation:{},releases:[{target:{session_id:'saved'},snapshot:{capture_id:'retired'}}]};
  const {pane,scope}=fixture([saved]);
  scope.DraftEditor=class {constructor(store,record){Object.assign(this,{store,record})} async flush(){}};
  pane.drainReferenceReleases=async(editor,_generation,retry)=>{assert.equal(retry,true);editor.record.releases=[];};
  await pane.restoreDrafts();let row=pane.recoveryView.rows[0];
  await row.open();assert.equal(row.canRecreate,false);
  assert.equal(typeof row.cleanup,'function');await row.cleanup();
  row=pane.recoveryView.rows[0];await row.open();assert.equal(row.canRecreate,true);
});

test('expired Fresh references retire durably before full text can move to a replacement',async()=>{
  const store=await DraftStore.open('d'.repeat(32),{kind:'local'});
  try {
    const header='a'.repeat(64),workspace='b'.repeat(64);
    const blank=await store.ensure('main','expired',header,{workspace_id:workspace,session_id:'expired',agent_preset_id:null});
    const reference={snapshot:{capture_id:'capture',sha256:'c'.repeat(64),byte_len:1},preview:'Reference',metadata:{
      target:{session_id:'expired',header_sha256:header},source:{kind:'native',binding:{session_id:'source',header_sha256:header}},
      capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:header,scanned_bytes:9,omissions:[]}},text_bytes:9}};
    const editor=new DraftEditor(store,blank);
    const text='complete recovery text '.repeat(30);
    editor.edit(text,[],[reference]);await editor.flush();
    const {pane,scope}=fixture([]);scope.connection.drafts=store;scope.DraftEditor=DraftEditor;
    const released=[];
    pane.drainReferenceReleases=async(editor)=>{
      const durable=await store.get(...editor.record.key.slice(2));
      assert.equal(durable.references.length,0);assert.equal(durable.releases.length,1);
      for(const capture of durable.releases) {
        released.push(capture);
        await editor.update(record=>store.acknowledgeRelease(record,capture));
      }
    };
    await pane.restoreDrafts();let row=pane.recoveryView.rows[0];await row.open();
    assert.equal(row.text,text);assert.equal(row.canRecreate,false);
    await row.retireReferences();
    assert.deepEqual(released,[{target:reference.metadata.target,snapshot:reference.snapshot}]);
    row=pane.recoveryView.rows[0];await row.open();assert.equal(row.canRecreate,true);
    const saved=await store.get(...blank.key.slice(2));
    const replacement=await store.ensure('main','replacement',header,{workspace_id:workspace,session_id:'replacement',agent_preset_id:null});
    const moved=await store.moveFresh(saved,replacement);
    assert.equal(moved.text,text);assert.equal(moved.references.length,0);
  } finally {await clearDraftRecords()}
});
