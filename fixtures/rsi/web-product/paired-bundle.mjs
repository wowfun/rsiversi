import { readFile, realpath } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
export async function pairedBundle(environment = process.env) {
  const selectedBinary = environment.RSI_WEB_BINARY ?? environment.RSI_BINARY;
  const selectedAssets = environment.RSI_WEB_ASSETS;
  if (Boolean(selectedBinary) !== Boolean(selectedAssets)) throw new Error('Supply both RSI_WEB_BINARY and RSI_WEB_ASSETS from one paired publication');
  const bundle = resolve(environment.RSI_PAIRED_BUNDLE ?? join(root, 'target/rsi-app/current'));
  const binary = await realpath(selectedBinary ?? join(bundle, 'rsi')).catch(() => { throw new Error('Build paired artifacts first: pnpm -C apps/web build --debug'); });
  const assets = await realpath(selectedAssets ?? join(bundle, 'assets'));
  const receipt = JSON.parse(await readFile(join(dirname(binary), 'receipt.json'), 'utf8'));
  const bootstrap = JSON.parse(await readFile(join(assets, 'rsi-build.json'), 'utf8'));
  if (receipt.format !== 1 || !/^[a-f0-9]{64}$/.test(receipt.family_sha256) || bootstrap.family_sha256 !== receipt.family_sha256) throw new Error('Fixture binary and assets must belong to the same paired publication');
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(binary)) hash.update(bytes);
  if (hash.digest('hex') !== receipt.artifacts.rsi) throw new Error('Fixture binary digest differs from the publication receipt');
  return { binary, assets, receipt, family: receipt.family_sha256 };
}
