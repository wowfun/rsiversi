import test from 'node:test';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {fileURLToPath} from 'node:url';
test('real Worker module admission, cancellation and failure scenarios', async () => {
  try {
    await promisify(execFile)(process.execPath, ['--experimental-vm-modules', '--test',
      fileURLToPath(new URL('./support/worker-scenarios.mjs', import.meta.url))], {timeout: 15_000});
  } catch (error) {
    throw new Error(`${error.message}\n${error.stdout??''}\n${error.stderr??''}`, {cause:error});
  }
});
