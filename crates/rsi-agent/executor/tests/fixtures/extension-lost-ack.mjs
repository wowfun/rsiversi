import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function createExtension(options) {
  const original = await import(pathToFileURL(join(options.cohortRoot, 'node_modules/rsi-dsh-plugin-bridge/src/index.mjs')));
  const extension = await original.createExtension(options);
  let acknowledgements = 0, executions = 0;
  let mode = 'ack';
  const receipts=[];let abandonments=0;
  return { close: () => extension.close(), async handle(method, args, signal) {
    if (method === 'fixture.arm') { mode = args.mode; return null; }
    if (method === 'fixture.counts') return { acknowledgements, executions };
    if (method === 'fixture.rejections') return {receipts,abandonments};
    if (method === 'operation.abandon') abandonments++;
    if (method === 'agent.tool' || method === 'agent.command') executions++;
    const result = await extension.handle(method, args, signal);
    if (method === 'agent.command' && mode.startsWith('reject-') && executions === 1)
      result.inputs=Array.from({length:65},()=>({delivery:'inject',text:'invalid command fanout'}));
    if (method === 'operation.receipt' && mode.startsWith('reject-')) {
      receipts.push(args);
      if (args.outcome === 'rejected') return mode === 'reject-unloaded' ? {state:'unloaded'} : {state:'invalid'};
    }
    if ((method === 'agent.tool' || method === 'agent.command') && mode === 'stage' && executions === 1) throw new Error('fixture lost staged result after original execution');
    if (method === 'operation.receipt' && ++acknowledgements === 1 && mode === 'ack') throw new Error('fixture lost acknowledgement after original confirmation');
    return result;
  } };
}
