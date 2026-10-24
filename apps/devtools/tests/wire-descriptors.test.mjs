import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, writeFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {wireDescriptor} from '../wire-descriptors.mjs';

test('wire declarations enforce closed fields and the byte boundary', () => {
  const root = mkdtempSync(join(tmpdir(), 'wire-descriptor-'));
  const file = join(root, 'wire-contract.json');
  try {
    assert.throws(() => wireDescriptor(file), /ENOENT/);
    const json = JSON.stringify({name:'fixture.wire',version:1});
    writeFileSync(file, json.padEnd(1024, ' '));
    assert.deepEqual(wireDescriptor(file), {name:'fixture.wire',version:1});
    writeFileSync(file, json.padEnd(1025, ' '));
    assert.throws(() => wireDescriptor(file), /capacity/);
    writeFileSync(file, JSON.stringify({name:'fixture.wire',version:1,extra:true}));
    assert.throws(() => wireDescriptor(file), /Invalid/);
    writeFileSync(file, Buffer.alloc(8 * 1024 * 1024));
    assert.throws(() => wireDescriptor(file), /capacity/);
  } finally { rmSync(root, {recursive:true,force:true}); }
});
