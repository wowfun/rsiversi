import test from 'node:test';
import assert from 'node:assert/strict';
import { bootstrapFiles, rendererFiles, upstreamAssetPattern } from '../bundle-contract.mjs';

test('development proxies every fixed upstream asset and keeps document overrides local', () => {
  const route = new RegExp(upstreamAssetPattern);
  for (const {name, stage} of bootstrapFiles) {
    assert.equal(route.test(`/${name}`), stage !== 'document', name);
    assert.equal(route.test(`/${name}?v=1`), stage !== 'document', name);
    assert.equal(route.test(`/${name}/source.rs`), false);
  }
  assert.equal(route.test('/ui-renderers.json'), true);
  for (const name of rendererFiles.filter(name => name !== 'ui-renderers.json')) assert.equal(route.test(`/${name}`), false);
  for (const path of ['/Cargo.lock', '/target/debug/rsi', '/.env', '/workerXjs']) assert.equal(route.test(path), false);
});
