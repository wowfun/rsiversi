import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile} from 'node:fs/promises';
import {test} from 'node:test';

const root = new URL('../vendor/dsh/', import.meta.url);
test('vendored DSH bytes retain their reviewed provenance', async () => {
  const manifest = JSON.parse(await readFile(new URL('provenance.json', root), 'utf8'));
  assert.match(manifest.revision, /^[a-f0-9]{40}$/);
  assert(manifest.files.some(file => file.path === 'LICENSE'));
  const seen = new Set();
  for (const file of manifest.files) {
    assert(!seen.has(file.path), `duplicate provenance: ${file.path}`);
    seen.add(file.path);
    const digest = createHash('sha256').update(await readFile(new URL(file.path, root))).digest('hex');
    assert.equal(digest, file.adapted_sha256, file.path);
  }
});
