import test from 'node:test';
import assert from 'node:assert/strict';
import {cdpPacket,decodeCdp} from './cdp.mjs';
test('opaque CDP JSON preserves private UTF-16 through a scalar JSON envelope',()=>{
  const value={id:7,result:{attributes:['aria-label','target\ud800'],value:'text\udfff',valid:'😀'}};
  const wire=JSON.stringify(cdpPacket(value));
  assert.ok(wire.isWellFormed());
  assert.ok(wire.includes('\\\\ud800'));
  const packet=JSON.parse(wire);
  assert.equal(typeof packet.value,'string');
  assert.deepEqual(decodeCdp(packet.value),value);
});
test('CDP decoder refuses malformed private envelopes',()=>{
  for(const value of [null,{},[],42,'null','[]','"text"','{"id":1}\0','{'])assert.throws(()=>decodeCdp(value));
});
