import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile, readdir} from 'node:fs/promises';
import {relative,join,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
import {test} from 'node:test';
import {execFileSync} from 'node:child_process';

const root = new URL('../vendor/dsh/', import.meta.url);
test('vendored DSH bytes retain their reviewed provenance', async () => {
  const manifest = JSON.parse(await readFile(new URL('provenance.json', root), 'utf8'));
  assert(manifest.files.some(file => file.path === 'LICENSE'));
  assert.deepEqual([...new Set(manifest.files.map(file=>file.revision))], ['22ddee3792402c3e70ec5f86a63c3e3e895a2f60']);
  const upstream=process.env.RSI_DSH_REFERENCE;
  if(upstream) assert.equal(execFileSync('git',['-C',upstream,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),manifest.files[0].revision);
  const actual = (await readdir(root, {recursive:true,withFileTypes:true})).filter(entry => entry.isFile()).map(entry => relative(fileURLToPath(root),join(entry.parentPath,entry.name)).split(sep).join('/')).filter(path => path !== 'provenance.json').sort();
  assert.deepEqual(manifest.files.map(file => file.path).sort(), actual);
  const seen = new Set();
  for (const file of manifest.files) {
    assert.match(file.revision, /^[a-f0-9]{40}$/);
    assert(!seen.has(file.path), `duplicate provenance: ${file.path}`);
    seen.add(file.path);
    assert.match(file.source_sha256, /^[a-f0-9]{64}$/);
    assert(!file.path.split("/").includes(".."));
    const digest = createHash('sha256').update(await readFile(new URL(file.path, root))).digest('hex');
    assert.equal(digest, file.adapted_sha256, file.path);
    if(upstream) assert.equal(createHash('sha256').update(await readFile(join(upstream,file.source))).digest('hex'),file.source_sha256,`upstream ${file.source}`);
  }
});
