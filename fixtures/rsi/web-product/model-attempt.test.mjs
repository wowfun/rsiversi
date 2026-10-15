import assert from 'node:assert/strict';
import test from 'node:test';
import {readFile} from 'node:fs/promises';
import {attemptEvents, modelEvents} from './model-attempt.mjs';

test('compact v1 uses the Rust and Python golden with exact Unicode, clocks and terminal', async () => {
  const fixture = JSON.parse(await readFile(new URL('../../../crates/rsi-agent/session-protocol/tests/fixtures/model-attempt-v1.json', import.meta.url), 'utf8'));
  const records = [...attemptEvents(fixture)];
  assert.deepEqual(records.map(({elapsed_ms,event}) => [Number(elapsed_ms),event]), [
    [10,{type:'content_started',index:0,content:{type:'text'}}],
    [12,{type:'content_delta',index:0,delta:{type:'text',value:'界🦀'}}],
    [20,{type:'content_finished',index:0}],
    [22,{type:'finished',reason:'max_tokens',replay:null}],
  ]);
  const fact = {type:'model_attempt_settled',seq:8,turn_id:'turn',effect_id:'effect',purpose:'conversation',attempt:fixture};
  const raw = JSON.stringify(fact);
  assert([...modelEvents([fact])].every((record,index) => record.seq === 8 && record.event_index === index));
  assert.equal(JSON.stringify(fact),raw);
  for (const bad of [{...fixture,v:2},{...fixture,chunks:[]},{...fixture,chunks:['!']},{...fixture,extra:true}]) {
    assert.throws(() => [...attemptEvents(bad)]);
  }
});
