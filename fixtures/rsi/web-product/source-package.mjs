import { assemble } from '../../../apps/devtools/packages.mjs';
import { rm } from 'node:fs/promises';
import { isAbsolute, resolve, dirname, basename } from 'node:path';

const [assets, output] = process.argv.slice(2);
if (!assets || !output || !isAbsolute(assets) || !isAbsolute(output)
    || dirname(resolve(output)) !== process.cwd() || basename(output) !== 'web-package') {
  throw new Error('Renderer fixture output must be its isolated web-package directory');
}
await rm(output, {recursive:true, force:true});
await assemble('web', output, assets);
