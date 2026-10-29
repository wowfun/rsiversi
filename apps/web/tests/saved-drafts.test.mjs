import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';

const record={key:['endpoint','local','main','saved'],header:'header',text:'',images:[],references:[],pending:null,creation:{},everDispatched:false};
function fixture(records=[record]) {
  const calls=[],scope={connection:{drafts:{list:async()=>records}},call:async(_method,payload)=>{calls.push(JSON.parse(payload));return '{"status":"expired"}'}};
  const Pane=createPaneClass(scope),pane=new Pane('main');
  pane.flush=async()=>{};pane.renderComposer=()=>{};
  return {pane,scope,calls};
}

test('reference-only saved input is retained as nonempty; expired fresh input offers explicit recreation',async()=>{
  const {pane}=fixture([{...record,references:[{snapshot:{capture_id:'retained'}}]}, {...record,key:['endpoint','local','main','dispatched'],everDispatched:true}]);
  await pane.restoreDrafts();const [first,second]=pane.recoveryView.rows;
  assert.equal(first.empty,false);
  await first.open();assert.equal(first.expired,true);assert.equal(first.canRecreate,true);
  await second.open();assert.equal(second.expired,true);assert.equal(second.canRecreate,false);
});

test('closing recovery during draft flush fences its queued open request',async()=>{
  const {pane,calls}=fixture();let release;
  pane.flush=()=>new Promise(resolve=>release=resolve);
  await pane.restoreDrafts();const panel=pane.recoveryView,opening=panel.rows[0].open();
  panel.close();release();await opening;
  assert.equal(calls.length,0);assert.equal(pane.recoveryView,undefined);
});

test('a retired recovery reply cannot change the replacement connection switching state',async()=>{
  const {pane,scope}=fixture();let release;
  scope.call=()=>new Promise(resolve=>release=resolve);
  await pane.restoreDrafts();const panel=pane.recoveryView,opening=panel.rows[0].open();
  await Promise.resolve();await Promise.resolve();
  scope.connection={};pane.switching=true;release('{"status":"expired"}');await opening;
  assert.equal(pane.switching,true);assert.equal(panel.rows[0].expired,undefined);
});
