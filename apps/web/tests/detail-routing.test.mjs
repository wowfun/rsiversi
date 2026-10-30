import test from 'node:test';
import assert from 'node:assert/strict';
import {createViewProjector} from '../src/runtime/view-projector.js';
import {createResourceProjector} from '../src/runtime/resource-projector.js';
import {SettingsDialog} from '../settings-dialog.js';

function fixture() {
  const registered = [], modal = [], resources = [], owner = {};
  const modalTarget = {clear(){ modal.push(null); }};
  const registeredTarget = {clear(){ registered.push(null); }};
  const settingsDialog = new SettingsDialog({close(){}, send(){}, failed(){}});
  const scope = {connection:owner, settingsDialog, modalTarget, registeredTarget, panes:new Map(), selected:undefined,
    lastNotice:undefined, notify(){}, select(){}, publish(){}, publishSurfaces(){},
    renderSettings(next){if(settingsDialog.update(owner,next.settings_revision)&&(next.settings_catalog||next.settings))registered.push(next)}, renderResources(next){ resources.push(next.panels); }};
  scope.render=createViewProjector(scope);
  const render = fields => scope.render({surfaces:{}, catalog:{models:[]}, ...fields});
  return {render, registered, modal, resources, settingsDialog, owner};
}

test('dismissed Settings does not reopen while independent Dock panels update', () => {
  const f=fixture();
  f.render({settings_revision:'1',settings_catalog:{ticket:'1'}});
  f.settingsDialog.dismiss();
  const before=f.registered.length;
  f.render({settings_revision:'1',settings_catalog:{ticket:'1'}});
  assert.equal(f.registered.length,before);
  f.render({settings_revision:'2',settings_catalog:{ticket:'2'}});
  assert.equal(f.registered.at(-1)?.settings_catalog.ticket,'2');
  assert.equal(f.modal.length,0);
});

test('current Rust resource-panel interactions remain independent of dismissed Settings', () => {
  const f = fixture();
  f.settingsDialog.update(f.owner, '1'); f.settingsDialog.dismiss();
  const panels = [{view:'approval-view', detail:{kind:'approval', request:{id:'a'}}}];
  f.render({settings_revision:'1', panels});
  assert.equal(f.resources.at(-1), panels);
});

test('resource projection accepts empty surfaces and observes generation changes once', () => {
  const observed=[];
  const scope={connection:{resources:{observe:(panels,surfaces)=>observed.push({panels,surfaces})}},resourceHosts:new Map(),publishResources(){}};
  scope.renderResources=createResourceProjector(scope);
  const panels=[];
  scope.renderResources({panels,surfaces:{main:null,side:null}});
  scope.renderResources({panels,surfaces:{main:null,side:null}});
  assert.equal(observed.length,1);
  scope.renderResources({panels,surfaces:{main:{generation:'1'},side:null}});
  assert.equal(observed.length,2);
  scope.renderResources({panels,surfaces:{main:null,side:null}});
  assert.equal(observed.length,3);
});

test('unchanged panels still contribute renderer slots to each incremental frame',()=>{
  const slots=[],panels=[{view:'card'}];let observed=0;
  const target={entry:panels[0]},scope={connection:{resources:{observe(){observed++}}},resourceHosts:new Map([['card',target]]),publishResources(){},renderDetail(_entry,host){slots.push(host)}};
  const render=createResourceProjector(scope),view={panels,surfaces:{main:{generation:'1'}}};
  render(view);assert.deepEqual(slots,[target]);slots.length=0;
  render(view);assert.deepEqual(slots,[target]);assert.equal(observed,1);
});
