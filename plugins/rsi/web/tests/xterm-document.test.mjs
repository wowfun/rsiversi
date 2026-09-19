import assert from 'node:assert/strict';
import {test} from 'node:test';
import {createRequire} from 'node:module';
import {dirname,resolve} from 'node:path';
import {xtermDocumentOverride} from '../xterm-document.mjs';
const require=createRequire(import.meta.url);
const manifest=require.resolve('@xterm/xterm/package.json');
const entry=resolve(dirname(manifest),require(manifest).module);
test('resolved xterm module is patched with Vite queries and either path separator',()=>{
  for(const id of [entry,entry+'?v=fixture',entry.replaceAll('/','\\')]){
    const plugin=xtermDocumentOverride();
    plugin.configResolved({command:'build'});plugin.buildStart();
    const source=plugin.load(id);
    assert.ok(source.includes('this._document??(typeof window<"u"?window.document:null)'));
    plugin.buildEnd();
  }
});
test('a production build that bypasses the patch fails closed',()=>{
  const plugin=xtermDocumentOverride();
  plugin.configResolved({command:'build'});plugin.buildStart();
  assert.equal(plugin.load('/unrelated/module.js'),undefined);
  assert.throws(()=>plugin.buildEnd(),/did not load/);
});
test('development resolution also refuses an unpatched xterm entry',async()=>{
 const plugin=xtermDocumentOverride();plugin.configResolved({command:'serve'});
 await assert.rejects(plugin.resolveId.call({resolve:async()=>({id:'/wrong/xterm.js'})},'@xterm/xterm'),/outside the reviewed/);
 assert.deepEqual(await plugin.resolveId.call({resolve:async()=>({id:entry})},'@xterm/xterm'),{id:entry});
});
