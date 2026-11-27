import assert from 'node:assert/strict';
import {test} from 'node:test';
import fs from 'node:fs';
import {createRequire,syncBuiltinESMExports} from 'node:module';
import {dirname,resolve} from 'node:path';
import {xtermDocumentOverride} from '../xterm-document.mjs';
const require=createRequire(import.meta.url);
const manifest=require.resolve('@xterm/xterm/package.json');
const entry=resolve(dirname(manifest),require(manifest).module);
test('unrelated source edits can build while a missing or duplicated patch site is rejected',t=>{
  const source=fs.readFileSync(entry,'utf8'),site='this._document??typeof window<"u"?window.document:null';
  const read=fs.readFileSync;
  let candidate=source+'\n// unrelated renderer annotation';
  const mocked=t.mock.method(fs,'readFileSync',function(path,...args){return path===entry?candidate:read.call(this,path,...args);});
  syncBuiltinESMExports();
  try{
    assert.ok(xtermDocumentOverride().load(entry).endsWith('// unrelated renderer annotation'));
    candidate=source.replace(site,'changed');
    assert.throws(xtermDocumentOverride,/patches no longer apply exactly once/);
    candidate=source+site;
    assert.throws(xtermDocumentOverride,/patches no longer apply exactly once/);
  }finally{mocked.mock.restore();syncBuiltinESMExports();}
});
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
