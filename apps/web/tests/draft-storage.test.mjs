import assert from 'node:assert/strict';
import {test} from 'node:test';
import {DraftStore} from '../drafts.js';
test('unavailable storage preserves a usable blank editor and explains failed operations', async () => {
  const store = new DraftStore(undefined, 'a'.repeat(32), 'local');
  const blank = store.blank('main', 'session', 'b'.repeat(64));
  assert.equal(blank.text, '');
  for (const operation of [() => store.get('main', 'session'), () => store.list('main'), () => store.ensure('main','session','b'.repeat(64))]) {
    await assert.rejects(operation, /Draft storage is unavailable/);
  }
});
