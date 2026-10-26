import {test} from 'node:test';
import assert from 'node:assert/strict';
import {formUnavailable} from '../settings-editor.js';
test('form admission preserves exact numeric JSON and accepts safe values',()=>{
  for(const text of ['{"a":1,"unknown":{"x":"2e3"}}','[true,null,"-0",42]'])assert.equal(formUnavailable(text),'');
  for(const text of ['-0','1e3','1.0','9007199254740993','{"nested":[0.00]}'])assert.match(formUnavailable(text),/exact numbers/);
  assert.match(formUnavailable('{'),/valid JSON/);
});
