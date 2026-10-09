import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtemp, mkdir, writeFile, readFile, symlink, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {cleanupAll} from './cleanup.mjs';
import {redactEvidence, requireProviderUsage, liveTurnTerminal, settleLiveEvidence, recoverLiveRead} from './evidence.mjs';

test('live read recovery never replays the read or masks unclassified failures',async()=>{
  const timeout=new Error('read timed out');timeout.name='TimeoutError';
  let reads=0,recoveries=0;
  const read=async()=>{reads++;throw timeout;};
  assert.equal(await recoverLiveRead(read,async()=>{recoveries++;return true;}),undefined);
  assert.equal(reads,1);assert.equal(recoveries,1);
  await assert.rejects(recoverLiveRead(read,async()=>false),error=>error===timeout);
  const unknown=new Error('unclassified read failure');
  await assert.rejects(recoverLiveRead(async()=>{throw unknown;},async()=>{throw new Error('must not recover');}),error=>error===unknown);
  assert.deepEqual(await recoverLiveRead(async()=>({status:'Completed'}),()=>{throw new Error('must not recover');}),{status:'Completed'});
});

test('live cleanup closes every owner before scanning even after a close failure', async () => {
  const report = await mkdtemp(join(tmpdir(), 'rsi-live-cleanup-'));
  const key = 'fixture-cleanup-private-key', failure = new Error('close rejected');
  const closed = [];
  try {
    await assert.rejects(settleLiveEvidence(report, key,
      () => { closed.push('browser'); throw failure; },
      async () => { await writeFile(join(report, 'late.html'), key); closed.push('service'); },
      () => { closed.push('mcp'); },
    ), error => error instanceof AggregateError && error.errors.length === 2
      && error.errors[0].errors.includes(failure) && /1 files sanitized/.test(error.errors[1].message));
    assert.deepEqual([...closed].sort(), ['browser', 'mcp', 'service']);
    assert.equal(await readFile(join(report, 'late.html'), 'utf8'), '[REDACTED]');
  } finally { await rm(report, {recursive: true, force: true}); }
});

test('a completed turn with different human text cannot acknowledge the requested prompt', () => {
  const facts = [
    {seq: 1, type: 'input_message_entered', turn_id: 'other', source: {type: 'human'}, content: [{type: 'text', text: 'different'}]},
    {seq: 2, type: 'turn_terminal', turn_id: 'other', outcome: {status: 'completed'}},
  ];
  assert.equal(liveTurnTerminal(facts, 'requested', 0), undefined);
});

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

test('a previous terminal or another Turn cannot acknowledge the submitted live input', () => {
  const old = {seq: 2, type: 'turn_terminal', turn_id: 'old', outcome: {status: 'completed'}};
  const input = {seq: 3, type: 'input_message_entered', turn_id: 'new', source: {type: 'human'}, content: [{type: 'text', text: 'next'}]};
  const unrelated = {seq: 4, type: 'turn_terminal', turn_id: 'other', outcome: {status: 'completed'}};
  assert.equal(liveTurnTerminal([old], 'next', 2), undefined);
  assert.equal(liveTurnTerminal([old, input, unrelated], 'next', 2), undefined);
  const terminal = {seq: 5, type: 'turn_terminal', turn_id: 'new', outcome: {status: 'completed'}};
  assert.equal(liveTurnTerminal([old, input, unrelated, terminal], 'next', 2), terminal);
});

test('live matching handles repeated prompts, immediate completion and failed outcomes', () => {
  const input = (seq, turn_id, source = 'human') => ({seq, type: 'input_message_entered', turn_id, source: {type: source}, content: [{type: 'text', text: 'repeat'}]});
  const terminal = (seq, turn_id, status) => ({seq, type: 'turn_terminal', turn_id, outcome: {status}});
  const prior = [input(1, 'old'), terminal(2, 'old', 'completed')];
  assert.equal(liveTurnTerminal(prior, 'repeat', 2), undefined);
  const current = terminal(4, 'new', 'completed');
  assert.equal(liveTurnTerminal([...prior, input(3, 'new'), current], 'repeat', 2), current);
  const failed = terminal(4, 'new', 'failed');
  assert.equal(liveTurnTerminal([...prior, input(3, 'new'), failed], 'repeat', 2), failed);
  assert.equal(liveTurnTerminal([...prior, input(3, 'context', 'plugin_context'), terminal(4, 'context', 'completed')], 'repeat', 2), undefined);
  const repeated = [...prior, input(3, 'first'), terminal(4, 'first', 'completed'), input(5, 'second')];
  assert.equal(liveTurnTerminal(repeated, 'repeat', 2), undefined);
  const latest = terminal(6, 'second', 'failed');
  assert.equal(liveTurnTerminal([...repeated, latest], 'repeat', 2), latest);
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
