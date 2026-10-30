import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
import {performanceEntrypoint} from './performance-input.mjs';

test('performance evidence hashes each original entry without requiring legacy app.js in current source',async()=>{
  const root=await mkdtemp(join(tmpdir(),'rsi-performance-entry-'));
  try {
    for(const [variant,path,text] of [['baseline','app.js','legacy entry'],['current','src/main.tsx','React entry']]) {
      const source=join(root,variant);await mkdir(join(source,'src'),{recursive:true});await writeFile(join(source,path),text);
      const {bytes,identity}=await performanceEntrypoint(source,variant);
      assert.equal(bytes.toString(),text);
      assert.deepEqual(identity,{path,sha256:createHash('sha256').update(text).digest('hex')});
    }
  } finally {await rm(root,{recursive:true,force:true});}
});
