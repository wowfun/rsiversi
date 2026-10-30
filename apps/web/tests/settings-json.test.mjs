import {test} from 'node:test';
import assert from 'node:assert/strict';
import {formUnavailable, settingsLimitExceeded, SETTINGS_LIMIT_MESSAGE} from '../settings-json.js';
import {createSettingsFields} from '../src/runtime/settings-fields.js';
test('form admission preserves exact numeric JSON and accepts safe values',()=>{
  for(const text of ['{"a":1,"unknown":{"x":"2e3"}}','[true,null,"-0",42]'])assert.equal(formUnavailable(text),'');
  for(const text of ['-0','1e3','1.0','9007199254740993','{"nested":[0.00]}'])assert.match(formUnavailable(text),/exact numbers/);
  assert.match(formUnavailable('{'),/valid JSON/);
});
test('form and JSON editing share exact UTF-8 boundary admission',()=>{
  for (const glyph of ['a','界','😀','\ud800']) {
    for(const count of [100,Math.floor(1048574/new TextEncoder().encode(glyph).length),350000]) {
      const text='"'+glyph.repeat(count)+'"';
      const exceeded=new TextEncoder().encode(text).length>1048576;
      assert.equal(settingsLimitExceeded(text),exceeded);
      const fields=createSettingsFields({type:'string'},text,false,()=>{});
      assert.equal(fields.initialOversized,exceeded);
      assert.equal(formUnavailable(text)===SETTINGS_LIMIT_MESSAGE,exceeded);
      if(exceeded) assert.throws(()=>fields.json(),/1 MiB/);
      else assert.equal(fields.json(),text);
    }
  }
});
