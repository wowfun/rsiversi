import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function createExtension(options) {
  const original = await import(pathToFileURL(join(options.cohortRoot, 'node_modules/rsi-dsh-plugin-bridge/src/index.mjs')));
  const extension = await original.createExtension(options);
  const version = '__GENERATION__';
  const rejectState = __REJECT_STATE__;
  const loads = [];
  let loading;
  let tools = 0, finished = 0;
  return { close: () => extension.close(), async handle(method, args, signal) {
    if (method === 'fixture.generation') return { version, loads, tools, finished };
    if (method === 'agent.load.begin') {
      loading = { revision: args.revision, count: args.count, reload: args.reload, todos: 0 };
      loads.push(loading);
    }
    if (method === 'agent.load.events') loading.todos += args.events.filter(event => event.type === 'todo/write').length;
    if (method === 'agent.load.finish') {
      if (rejectState && args.checkpoint?.schemaVersion !== 2) throw new Error('replacement requires checkpoint schema 2');
      finished++;
    }
    if (method === 'agent.tool') tools++;
    return extension.handle(method, args, signal);
  } };
}
