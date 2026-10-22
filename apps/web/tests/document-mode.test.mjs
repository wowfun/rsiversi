import assert from 'node:assert/strict';
import {test} from 'node:test';
import {isDesktop} from '../desktop-bridge.js';
import {createDocumentTransport} from '../src/native.ts';

test('Desktop marker never changes the Worker transport or grants native authority alone', () => {
  const original = globalThis.Worker, workers = [];
  globalThis.Worker = class { constructor(url, options) { workers.push([url, options]); } };
  try {
    assert.equal(isDesktop({__RSI_DESKTOP__:true}),false);
    assert.equal(isDesktop({__TAURI_INTERNALS__:{invoke(){}}}),false);
    assert.equal(isDesktop({__RSI_DESKTOP__:true,__TAURI_INTERNALS__:{invoke(){}}}),true);
    for (const desktop of [true,false]) {
      globalThis.__RSI_DESKTOP__ = desktop;
      assert.ok(createDocumentTransport() instanceof globalThis.Worker);
    }
    assert.deepEqual(workers,Array(2).fill(['/worker.js',{type:'module'}]));
  } finally { globalThis.Worker = original; delete globalThis.__RSI_DESKTOP__; }
});
