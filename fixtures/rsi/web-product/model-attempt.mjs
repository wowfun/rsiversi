// Read-only fixture decoding of the Session protocol's compact v1 evidence.
// Derived events never replace canonical Facts in hashes or Store operations.
import assert from 'node:assert/strict';
const rawBlock = (192 * 1024 - 4) / 4 * 3;
const maximum = 32 * 1024 * 1024 - 4096;
const utf8 = new TextDecoder('utf-8', {fatal: true});
export function* attemptEvents(attempt) {
  assert.deepEqual(Object.keys(attempt).sort(), ['chunks', 'retain_context', 'v']);
  assert.equal(attempt.v, 1); assert.equal(typeof attempt.retain_context, 'boolean');
  assert(Array.isArray(attempt.chunks) && attempt.chunks.length > 0);
  let size = 64;
  const chunks = attempt.chunks.map((chunk, index) => {
    assert.equal(typeof chunk, 'string'); assert(chunk.length + 3 <= 256 * 1024);
    size += chunk.length + 3; assert(size <= maximum);
    const data = Buffer.from(chunk, 'base64');
    assert(data.length > 0); assert.equal(data.toString('base64'), chunk);
    if (index + 1 < attempt.chunks.length) assert.equal(data.length, rawBlock);
    return data;
  });
  const data = Buffer.concat(chunks); let offset = 0;
  const byte = () => { assert(offset < data.length, 'truncated attempt'); return data[offset++]; };
  function integer() {
    let value = 0n;
    for (let shift = 0n; shift < 70n; shift += 7n) {
      const part = byte(); if (shift === 63n) assert(part <= 1);
      value |= BigInt(part & 127) << shift;
      if (part < 128) { assert(shift === 0n || part !== 0, 'nonminimal integer'); return value; }
    }
    assert.fail('invalid integer');
  }
  let elapsed = 0n; let terminal = false;
  while (offset < data.length) {
    assert(!terminal, 'event after terminal'); elapsed += integer(); assert(elapsed <= (1n << 64n) - 1n);
    const tag = byte(); assert(tag <= 3);
    const index = tag ? integer() : 0n; assert(index <= 0xffffffffn);
    const length = integer(); assert(length <= BigInt(maximum) && BigInt(offset) + length <= BigInt(data.length));
    const text = utf8.decode(data.subarray(offset, offset + Number(length))); offset += Number(length);
    const event = tag ? {type:'content_delta', index:Number(index), delta:{type:['text','reasoning','tool_arguments'][tag-1],value:text}} : JSON.parse(text);
    if (!tag) {
      assert(['content_started','content_finished','usage','source','warning','finished','failed'].includes(event.type));
      // The Rust serializer uses exact u64 values. Do not round unrepresentable evidence.
      assert.equal(JSON.stringify(event), text, 'noncanonical or inexact JSON event');
    }
    terminal = event.type === 'finished' || event.type === 'failed';
    yield {elapsed_ms: elapsed.toString(), event};
  }
  assert(terminal, 'missing attempt terminal');
}
export function* modelEvents(facts) {
  for (const fact of facts) {
    if (fact.type !== 'model_attempt_settled') continue;
    let event_index = 0;
    for (const record of attemptEvents(fact.attempt)) {
      yield {type:'attempt_event', seq:fact.seq, turn_id:fact.turn_id, effect_id:fact.effect_id, purpose:fact.purpose, event_index:event_index++, ...record};
    }
  }
}
export function assistantText(facts) {
  return [...modelEvents(facts)].filter(record => record.purpose === 'conversation' && record.event.type === 'content_delta' && record.event.delta.type === 'text').map(record => record.event.delta.value).join('');
}
