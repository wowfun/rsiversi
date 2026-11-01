import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {DocumentConnection} from '../../document-connection.js';
import {limits} from '../../admission.js';

const tick = () => new Promise(resolve => setImmediate(resolve));
const gate = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {resolve = yes; reject = no;});
  return {promise, resolve, reject};
};
const names = ['default','build_family','connect','connect_local','command','resource_open','terminal','restore_session','prepare_submission','export_input','reference_input','file_input','directory_input','automation_artifact','dispatch_submission','ui_source','import_image','read_image','next_view','acknowledge_frame','commit_renderer','disconnect','resource_snapshot'];
async function fixture(overrides = {}) {
  const view = gate(), messages = [], failures = [];
  let terminated = 0, disposed = 0;
  const transport = {
    postMessage: data => queueMicrotask(() => context.self.onmessage({data})),
    terminate() {terminated++;},
  };
  const context = vm.createContext({self: {}, setTimeout, clearTimeout, Uint8Array,
    postMessage(data) {messages.push(data); queueMicrotask(() => transport.onmessage({data}));},
  });
  const exports = {default: async () => {}, build_family: () => '__RSI_BUILD_FAMILY__',
    connect: async () => '{}', next_view: () => view.promise,
    disconnect: async () => '{}', ...overrides};
  const wasm = new vm.SyntheticModule(names, function () {
    for (const name of names) this.setExport(name, exports[name] ?? (() => {throw Error(`Unexpected WASM call: ${name}`);}));
  }, {context});
  const worker = new vm.SourceTextModule(await readFile(new URL('../../worker.js', import.meta.url), 'utf8'), {context});
  const admission = new vm.SourceTextModule(await readFile(new URL('../../admission.js', import.meta.url), 'utf8'), {context});
  await worker.link(specifier => {
    if (specifier === './admission.js') return admission;
    if (specifier === '/rsi_web.js') return wasm;
    throw Error(`Unexpected module: ${specifier}`);
  });
  await worker.evaluate();
  const owner = new DocumentConnection({transport, mounts: {close: async () => {disposed++;}, render() {}},
    isCurrent: () => true, onFrame: async () => ({accepted: true}), onFailure: error => failures.push(error.message)});
  await owner.authenticate({buildFamily: '__RSI_BUILD_FAMILY__'}, async () => ({}));
  return {owner, view, messages, failures, context, transport,
    get terminated() {return terminated;}, get disposed() {return disposed;}};
}

test('Rust admission metadata distinguishes refusal from unknown outcome', async () => {
  let failure;
  const f = await fixture({command: async () => {throw failure;}});
  for (const [error, notAdmitted, retryable] of [
    [Object.assign(Error('busy'), {notAdmitted: true}), true, true],
    [Error('unknown admitted outcome'), false, false],
    [Object.assign(Error('closed'), {notAdmitted: true, retryable: false}), true, false],
  ]) {
    failure = error;
    await assert.rejects(f.owner.request('command', '{}'), result => result.notAdmitted === notAdmitted && result.retryable === retryable);
  }
  await f.owner.retire();
});

for (const method of ['export_input', 'command']) test(`${method} cancellation reaches its owner while ordinary is full`, async () => {
  const blocked = gate(); let cancelled = 0;
  const cancel = method === 'export_input'
    ? JSON.stringify({pane: 'main', generation: '1', operation: {kind: 'cancel', token: 'export'}})
    : JSON.stringify({action: 'close_detail', view: '1', expected_open_revision: '1'});
  const f = await fixture({command: payload => payload === cancel ? (++cancelled, Promise.resolve()) : blocked.promise,
    export_input: async () => {cancelled++; return '{}';}});
  const pending = Array.from({length: limits.ordinary}, () => f.owner.request('command', '{}'));
  await tick();
  await assert.rejects(f.owner.request('command', '{}'), error => error.notAdmitted);
  await f.owner.request(method, cancel);
  assert.equal(cancelled, 1);
  // Delivery alone cannot return slots still owned by the blocked operation.
  await assert.rejects(f.owner.request('command', '{}'), error => error.notAdmitted);
  blocked.resolve(); await Promise.all(pending); await tick();
  await f.owner.request('command', '{}');
  await f.owner.retire();
});

for (const cleanupRejects of [false, true]) test(`view failure settles a full document despite ${cleanupRejects ? 'rejected' : 'blocked'} cleanup`, async () => {
  const blocked = gate(); let mutations = 0, settled = 0;
  const f = await fixture({command: () => {mutations++; return blocked.promise;},
    disconnect: () => cleanupRejects ? Promise.reject(Error('cleanup failed')) : new Promise(() => {})});
  const pending = Array.from({length: limits.ordinary}, () => f.owner.request('command', '{}').catch(error => {settled++; assert.equal(error.notAdmitted, undefined);}));
  await tick(); f.view.reject(Error('injected view failure')); await tick();
  await Promise.all(pending); await f.owner.settled();
  assert.equal(f.owner.phase, 'Failed'); assert.equal(settled, 8);
  assert.equal(f.terminated, 1); assert.equal(f.disposed, 1); assert.equal(f.failures.length, 1);
  const fresh = await fixture({command: async () => 'fresh'});
  blocked.resolve('late'); await tick();
  assert.equal(mutations, 8); assert.equal(settled, 8); assert.equal(f.failures.length, 1);
  assert.equal(await fresh.owner.request('command', '{}'), 'fresh');
  assert.equal(fresh.owner.phase, 'Ready'); await fresh.owner.retire();
});

for (const [lane, method, payload] of [
  ['ordinary', 'command', '{}'],
  ['terminalRead', 'terminal', '{"request":{"type":"read"}}'],
  ['terminalWrite', 'terminal', '{"request":{"type":"write"}}'],
  ['exportControl', 'export_input', '{"operation":{"kind":"cancel"}}'],
]) test(`${lane} Worker capacity returns only after dispatch settles`, async () => {
  const blocked = gate();
  const f = await fixture({[method]: () => blocked.promise});
  for (let index = 0; index <= limits[lane]; index++) f.context.self.onmessage({data: {kind: 'call', id: 100 + index, method, payload}});
  await tick();
  assert.equal(f.messages.find(reply => reply.id === 100 + limits[lane]).notAdmitted, true);
  blocked.resolve('{}'); await tick();
  f.context.self.onmessage({data: {kind: 'call', id: 999, method, payload}}); await tick();
  assert.equal(f.messages.find(reply => reply.id === 999).result, '{}');
  await f.owner.retire();
});
