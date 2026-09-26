import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtemp, mkdir, writeFile, readFile, symlink, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {cleanupAll} from './cleanup.mjs';
import {redactEvidence, requireProviderUsage} from './evidence.mjs';

test('cleanup settles every owner after synchronous and asynchronous failures', async () => {
  const settled = [];
  await assert.rejects(cleanupAll(
    () => { settled.push('browser'); throw new Error('launch or close'); },
    async () => { settled.push('service'); throw new Error('service close'); },
    async () => { settled.push('provider'); },
  ), AggregateError);
  assert.deepEqual(settled, ['browser', 'service', 'provider']);
});

test('redaction sanitizes all nested evidence before reporting and ignores links', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rsi-evidence-'));
  try {
    const report = join(root, 'report'), browser = join(report, 'browser');
    await mkdir(browser, {recursive: true});
    const key = 'fixture-private-key';
    const paths = [join(report, 'a.log'), join(report, 'b.json'), join(browser, 'c.jsonl')];
    for (const path of paths) await writeFile(path, `${key} ${key}`);
    const external = join(root, 'external.txt'); await writeFile(external, key);
    await symlink(external, join(browser, 'linked.txt'));
    await assert.rejects(redactEvidence(report, key), /3 files sanitized, 0 files inaccessible/);
    for (const path of paths) assert.equal(await readFile(path, 'utf8'), '[REDACTED] [REDACTED]');
    assert.equal(await readFile(external, 'utf8'), key);
    await redactEvidence(report, key);
  } finally { await rm(root, {recursive: true, force: true}); }
});

test('live usage evidence cannot succeed on absent or invalid counts', () => {
  const facts = usage => [{type: 'model_event', event: {type: 'usage', usage}}];
  for (const value of [[], facts(null), facts({}), facts({input_tokens: 2, output_tokens: NaN}), facts({input_tokens: 0, output_tokens: 2})]) {
    assert.throws(() => requireProviderUsage(value));
  }
  assert.deepEqual(requireProviderUsage(facts({input_tokens: 12, output_tokens: 4})), [{input_tokens: 12, output_tokens: 4}]);
});


test('browser setup closes its owner if context creation fails', async () => {
  const {openBrowserPage} = await import('./browser-fixture.mjs');
  let closed = false;
  const failure = new Error('context failed');
  await assert.rejects(openBrowserPage({launch: async () => ({
    newContext: async () => { throw failure; },
    close: async () => { closed = true; },
  })}, []), error => error === failure);
  assert.equal(closed, true);
});

test('browser setup preserves both creation and cleanup failures', async () => {
  const {openBrowserPage} = await import('./browser-fixture.mjs');
  const creation = new Error('page failed'), cleanup = new Error('close failed');
  await assert.rejects(openBrowserPage({launch: async () => ({
    newContext: async () => ({newPage: async () => { throw creation; }}),
    close: async () => { throw cleanup; },
  })}, []), error => error instanceof AggregateError && error.errors[0] === creation && error.errors[1] === cleanup);
});
