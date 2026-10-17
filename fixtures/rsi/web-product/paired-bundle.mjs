import { readFile, realpath, open, lstat } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
export async function pairedBundle(environment = process.env) {
  const selectedBinary = environment.RSI_WEB_BINARY ?? environment.RSI_BINARY;
  const selectedAssets = environment.RSI_WEB_ASSETS;
  if (Boolean(selectedBinary) !== Boolean(selectedAssets)) throw new Error('Supply both RSI_WEB_BINARY and RSI_WEB_ASSETS from one paired publication');
  let bundle = environment.RSI_PAIRED_BUNDLE;
  if (!bundle && !selectedBinary) {
    const record = join(root, 'target/rsi-app/current');
    const info = await lstat(record);
    if (!info.isFile() || info.isSymbolicLink()) throw new Error('Current publication record must be an ordinary file');
    const file = await open(record, 'r');
    let current;
    try {
      // Application publication grammar: apps/devtools/README.md.
      const bytes = Buffer.alloc(22);
      const {bytesRead} = await file.read(bytes, 0, bytes.length, 0);
      current = bytes.subarray(0, bytesRead).toString('utf8');
    } finally { await file.close(); }
    if (!/^[0-9]{20}\n$/.test(current)) throw new Error('Invalid current publication identifier');
    bundle = join(root, 'target/rsi-app/generations', current.trim(), 'bundle');
  }
  bundle = resolve(bundle ?? dirname(selectedBinary));
  const name = process.platform === 'win32' ? 'rsi.exe' : 'rsi';
  const binary = await realpath(selectedBinary ?? join(bundle, name)).catch(() => { throw new Error('Build paired artifacts first: bash rsi.sh build --debug'); });
  const assets = await realpath(selectedAssets ?? join(bundle, 'assets'));
  const receipt = JSON.parse(await readFile(join(dirname(binary), 'receipt.json'), 'utf8'));
  const bootstrap = JSON.parse(await readFile(join(assets, 'rsi-build.json'), 'utf8'));
  if (receipt.format !== 2 || !/^[a-f0-9]{64}$/.test(receipt.family_sha256) || bootstrap.family_sha256 !== receipt.family_sha256) throw new Error('Fixture binary and assets must belong to the same paired publication');
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(binary)) hash.update(bytes);
  if (hash.digest('hex') !== receipt.artifacts[name]) throw new Error('Fixture binary digest differs from the publication receipt');
  return { binary, assets, receipt, family: receipt.family_sha256 };
}
