import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile,readdir} from 'node:fs/promises';
const web=new URL('../',import.meta.url);
test('the first-party sprite covers every used vendor glyph without mounting the catalog',async()=>{
  const exports=await readFile(new URL('vendor/dsh/dockkit-primitives/icons/index.tsx',web),'utf8');
  const mapping=new Map([...exports.matchAll(/export const (Icon\w+) = productIcon\('([^']+)', '([^']+)'\)/g)].map(([,name,glyph,weight])=>[name,`dsw-icon-${glyph}-${weight}`]));
  const used=new Set();
  for(const folder of ['src/','vendor/dsh/']) {
    const root=new URL(folder,web);
    for(const file of await readdir(root,{recursive:true})) {
      if(!/\.tsx?$/.test(file)||/(^|[\\/])icons[\\/]/.test(file))continue;
      const source=await readFile(new URL(file.replaceAll('\\','/'),root),'utf8');
      for(const [name] of source.matchAll(/\bIcon\w+\b/g))if(mapping.has(name))used.add(mapping.get(name));
    }
  }
  const sprite=await readFile(new URL('src/icon-sprite.tsx',web),'utf8');
  const mounted=new Set([...sprite.matchAll(/'(dsw-icon-[^']+)'/g)].map(([,id])=>id));
  assert.deepEqual([...mounted].sort(),[...used].sort());
  assert(mounted.size<mapping.size/2);
});
