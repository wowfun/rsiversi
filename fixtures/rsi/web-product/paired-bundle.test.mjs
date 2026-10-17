import assert from 'node:assert/strict';
import { test } from 'node:test';
import { copyFile, mkdir, mkdtemp, rm, writeFile, symlink } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

test('paired fixture reader accepts only the publisher identifier grammar', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rsi-publication-record-'));
  assert(root.startsWith(resolve(tmpdir()) + sep));
  try {
    const module = join(root, 'fixtures/rsi/web-product/paired-bundle.mjs');
    await mkdir(join(root, 'fixtures/rsi/web-product'), { recursive: true });
    await copyFile(new URL('./paired-bundle.mjs', import.meta.url), module);
    const { pairedBundle } = await import(pathToFileURL(module));
    const bundle = join(root, 'target/rsi-app/generations/00000000000000000001/bundle');
    await mkdir(join(bundle, 'assets'), { recursive: true });
    const name = process.platform === 'win32' ? 'rsi.exe' : 'rsi';
    const binary = Buffer.from('isolated fixture artifact');
    const family = 'a'.repeat(64);
    await writeFile(join(bundle, name), binary);
    await writeFile(join(bundle, 'assets/rsi-build.json'), JSON.stringify({ family_sha256: family }));
    await writeFile(join(bundle, 'receipt.json'), JSON.stringify({ format: 2, family_sha256: family,
      artifacts: { [name]: createHash('sha256').update(binary).digest('hex') } }));
    const current = join(root, 'target/rsi-app/current');
    await writeFile(current, '00000000000000000001\n');
    assert.equal((await pairedBundle({})).family, family);
    for (const record of ['5\n', '999999999999999999999999\n', '../outside\n',
      '00000000000000000001', '00000000000000000001\r\n', '00000000000000000001\nextra']) {
      await writeFile(current, record);
      await assert.rejects(pairedBundle({}), /Invalid current publication identifier/, record);
    }
    await rm(current);
    await mkdir(current);
    await assert.rejects(pairedBundle({}), /must be an ordinary file/);
    await rm(current, { recursive: true });
    if (process.platform !== 'win32') {
      // File symlinks require an optional Windows privilege; Linux exercises
      // the link boundary while both hosts reject non-file records above.
      await writeFile(join(root, 'record'), '00000000000000000001\n');
      await symlink(join(root, 'record'), current);
      await assert.rejects(pairedBundle({}), /must be an ordinary file/);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
