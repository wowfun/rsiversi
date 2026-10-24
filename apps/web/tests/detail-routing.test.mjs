import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {SettingsDialog} from '../settings-dialog.js';

const source = await readFile(new URL('../app.js', import.meta.url), 'utf8');
function fixture() {
  const rendered = [], resources = [], owner = {};
  const modalTarget = {clear(){ rendered.push(null); }};
  const settingsDialog = new SettingsDialog({close(){}, send(){}, failed(){}});
  const scope = {connection:owner, settingsDialog, modalTarget, panes:new Map(), selected:undefined,
    lastNotice:undefined, notify(){}, select(){}, publish(){},
    renderDetail(next){ rendered.push(next); }, renderResources(next){ resources.push(next.panels); }};
  vm.runInNewContext(source.slice(source.indexOf('function render(next)'), source.indexOf('installActions({')), scope);
  const render = fields => scope.render({surfaces:{}, catalog:{models:[]}, ...fields});
  return {render, rendered, resources, settingsDialog, owner};
}

test('dismissing Settings fences only that Settings window, not approval or question details', () => {
  const f = fixture();
  f.render({settings_revision:'1', settings_catalog:{ticket:'1'}});
  f.settingsDialog.dismiss();
  f.render({settings_revision:'1', settings_catalog:{ticket:'1'}});
  assert.equal(f.rendered.at(-1), null);
  for (const kind of ['approval', 'question']) {
    for (const settings_revision of ['1', '0']) {
      const detail = {kind, request:{id:kind}};
      f.render({settings_revision, detail});
      assert.equal(f.rendered.at(-1)?.detail, detail);
    }
  }
  f.render({settings_revision:'2', settings_catalog:{ticket:'2'}});
  assert.equal(f.rendered.at(-1)?.settings_catalog.ticket, '2');
});

test('current Rust resource-panel interactions remain independent of dismissed Settings', () => {
  const f = fixture();
  f.settingsDialog.update(f.owner, '1'); f.settingsDialog.dismiss();
  const panels = [{view:'approval-view', detail:{kind:'approval', request:{id:'a'}}}];
  f.render({settings_revision:'1', panels});
  assert.equal(f.resources.at(-1), panels);
});
