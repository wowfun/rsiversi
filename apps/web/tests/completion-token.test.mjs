import {test} from 'node:test';
import assert from 'node:assert/strict';
import {completionToken} from '../completion-token.js';

test('mention boundaries, suffix, locators and slash commands retain their meaning', () => {
  assert.equal(completionToken('hello @agent', 9)?.query, '@agent');
  for (const text of ['mail@agent', '@agent/file', '@path_hex:', '@a:value', '@_bad', '@'+'a'.repeat(65)])
    assert.equal(completionToken(text, text.indexOf('/') > 0 ? text.indexOf('/') : text.length), undefined, text);
  assert.equal(completionToken('/help argument', 3)?.query, 'help');
  for (const text of ['//help', '/help\n', '/help argument']) assert.equal(completionToken(text,text.length), undefined);
});

test('backtick runs preserve literal inline and nonoverlapping fence parity', () => {
  let seed = 731;
  for (let length = 0; length <= 128; length++) for (let sample = 0; sample < 100; sample++) {
    let prefix = '';
    for (let i = 0; i < length; i++) { seed = (Math.imul(seed,1664525)+1013904223) >>> 0; prefix += '`a \n/中'[seed % 6]; }
    const text = prefix + ' @agent';
    const suppressed = text.split('```').length % 2 === 0 || text.split('`').length % 2 === 0;
    assert.equal(!!completionToken(text,text.length), !suppressed, text);
  }
});

test('maximum draft preserves completion inside and outside literal code', () => {
  const body = 'x'.repeat(1024*1024-20);
  for (const [prefix, available] of [['',true],['`',false],['```',false],['``````',true]]) {
    const text = prefix + body + ' @agent';
    assert.equal(!!completionToken(text,text.length), available);
  }
});
