import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';

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
