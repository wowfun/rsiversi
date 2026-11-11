import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function createExtension(options) {
  const original = await import(pathToFileURL(join(options.cohortRoot, 'node_modules/rsi-dsh-plugin-bridge/src/index.mjs')));
  const extension = await original.createExtension(options);
  let preparations = 0;
  const receipts = [];
  return { close: () => extension.close(), async handle(method, args, signal) {
    if (method === 'fixture.step-receipts') return { preparations, receipts };
    if (method === 'agent.before-step') preparations++;
    const result = await extension.handle(method, args, signal);
    if (method === 'operation.receipt') receipts.push(args);
    return result;
  } };
}
