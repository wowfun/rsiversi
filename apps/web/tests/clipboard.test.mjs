import {test} from 'node:test';
import assert from 'node:assert/strict';
import {writeClipboard,setClipboardWriter} from '../mounts.js';
test('one bounded clipboard capability propagates native failures and never reports false success',async()=>{
  const writes=[];setClipboardWriter(async text=>writes.push(text));
  await writeClipboard('界'.repeat(21845));assert.equal(writes.length,1);
  await assert.rejects(writeClipboard('界'.repeat(21846)),/64 KiB/);assert.equal(writes.length,1);
  setClipboardWriter(async()=>{throw new Error('native rejected')});await assert.rejects(writeClipboard('code'),/native rejected/);
  setClipboardWriter(undefined);
});
