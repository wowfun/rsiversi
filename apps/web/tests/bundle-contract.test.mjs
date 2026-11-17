import test from 'node:test';
import assert from 'node:assert/strict';
import { bootstrapFiles, rendererFiles, upstreamAssetPattern } from '../bundle-contract.mjs';
import {build} from 'vite';
import {documentConfiguration} from '../vite.config.mjs';
import {fileURLToPath} from 'node:url';

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

test('bundled starting drafts and copied journal share the external database lifetime', async () => {
  const config = documentConfiguration({command:'build'});
  const entry = fileURLToPath(new URL('../starting-draft.js', import.meta.url));
  const output = await build({configFile:false,logLevel:'silent',
    build:{...config.build,write:false,rollupOptions:{...config.build.rollupOptions,input:entry,preserveEntrySignatures:'strict'}}});
  const chunk = output.output.find(item => item.type === 'chunk');
  assert.equal(chunk.imports.filter(name => name.endsWith('draft-databases.js')).length, 1);
  assert.match(chunk.code, /from[\s]*["']\.?\/draft-databases\.js["']/);
  assert.equal(Object.keys(chunk.modules).some(name => name.endsWith('/draft-databases.js')), false);
  assert.deepEqual(bootstrapFiles.filter(file => file.name === 'draft-databases.js'),
    [{name:'draft-databases.js',stage:'copy'}]);
});
