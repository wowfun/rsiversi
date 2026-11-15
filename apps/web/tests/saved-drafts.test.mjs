import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import 'fake-indexeddb/auto';
import {DraftStore, DraftEditor} from '../drafts.js';
globalThis.requestAnimationFrame = callback => queueMicrotask(callback);

const record={key:['endpoint','local','main','saved'],header:'header',text:'',images:[],references:[],releases:[],pending:null,creation:{},everDispatched:false};
function fixture(records=[record]) {
  const calls=[],scope={connection:{drafts:{namespace:['endpoint','local'],list:async()=>records}},call:async(_method,payload)=>{calls.push(JSON.parse(payload));return '{"status":"expired"}'}};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  pane.flush=async()=>{};pane.renderComposer=()=>{};pane.focusInput=()=>{};
  return {pane,scope,calls};
}

test('a failed first save remains discoverable only in its current namespace',async(t)=>{
  const store=await DraftStore.open('c'.repeat(32),{kind:'local'});
  try {
    const blank=await store.ensure('main','unsaved','a'.repeat(64),{workspace_id:'b'.repeat(64),session_id:'unsaved',agent_preset_id:null});
    const editor=new DraftEditor(store,blank);
    t.mock.method(store,'edit',async()=>{throw Error('disk full')});
    editor.edit('valuable unsaved text');await assert.rejects(editor.flush(),/disk full/);
    assert.equal((await store.list('main')).length,0);
    const {pane,scope}=fixture([]);scope.connection.drafts=store;
    pane.editors.set(JSON.stringify(blank.key),editor);
    pane.editors.set('foreign',{record:{...blank,key:['other','local','main','foreign']},dirty:true,text:'private'});
    await pane.restoreDrafts();
    assert.equal(pane.recoveryView.rows.length,1);
    assert.equal(pane.recoveryView.rows[0].unsaved.text,'valuable unsaved text');
  } finally {await DraftStore.discardAllSaved()}
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
  ]);
  await pane.restoreDrafts();
  assert.equal(pane.recoveryView.rows[0].summary,'saved · '+'x'.repeat(79)+'🚀');
  assert.equal(pane.recoveryView.rows[1].summary,'second · first second third');
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
  } finally {await DraftStore.discardAllSaved()}
});
