import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function createExtension(options) {
  const original = await import(pathToFileURL(join(options.cohortRoot, 'node_modules/rsi-dsh-plugin-bridge/src/index.mjs')));
  const extension = await original.createExtension(options);
  let acknowledgements = 0, executions = 0;
  let mode = 'ack';
  return { close: () => extension.close(), async handle(method, args, signal) {
    if (method === 'fixture.arm') { mode = args.mode; return null; }
    if (method === 'fixture.counts') return { acknowledgements, executions };
    if (method === 'agent.tool' || method === 'agent.command') executions++;
    const result = await extension.handle(method, args, signal);
    if ((method === 'agent.tool' || method === 'agent.command') && mode === 'stage' && executions === 1) throw new Error('fixture lost staged result after original execution');
    if (method === 'operation.receipt' && ++acknowledgements === 1 && mode === 'ack') throw new Error('fixture lost acknowledgement after original confirmation');
    return result;
  } };
}
