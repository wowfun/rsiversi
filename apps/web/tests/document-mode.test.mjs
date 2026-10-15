import assert from 'node:assert/strict';
import {test} from 'node:test';
import {isNativeDocument} from '../document-mode.js';
import {createDocumentTransport, NativeDocument} from '../src/native.ts';

test('exact desktop locations select native transport while browser origins construct a Worker', () => {
  const original = globalThis.Worker, workers = [];
  globalThis.Worker = class { constructor(url, options) { workers.push([url, options]); } };
  try {
    for (const href of ['rsi://localhost/', 'http://rsi.localhost/']) {
      const location = new URL(href);
      assert.equal(isNativeDocument(location), true);
      assert.ok(createDocumentTransport(location) instanceof NativeDocument);
    }
    for (const href of [
      'http://localhost/', 'http://127.0.0.1:8787/', 'https://example.org/',
      'https://rsi.localhost/', 'http://rsi.localhost:8787/',
      'http://rsi.localhost.evil/', 'rsi://localhost.evil/', 'rsi://localhost:8080/',
    ]) {
      const location = new URL(href);
      assert.equal(isNativeDocument(location), false, href);
      assert.ok(!(createDocumentTransport(location) instanceof NativeDocument));
    }
    assert.equal(workers.length, 8);
    for (const [url, options] of workers) {
      assert.equal(url, '/worker.js');
      assert.deepEqual(options, {type: 'module'});
    }
  } finally { globalThis.Worker = original; }
});
