import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function createExtension(options) {
  const source = join(options.cohortRoot, 'node_modules/rsi-dsh-plugin-bridge/src');
  const original = await import(pathToFileURL(join(source, 'index.mjs')));
  const { resolveBody } = await import(pathToFileURL(join(source, 'request-body.mjs')));
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
    if (method === 'agent.load.events') {
      const events = await resolveBody(args.events, 64 * 1024 * 1024, (token, offset) =>
        options.request('agent.request-body.read', { session_id: args.session_id, operation_id: 'load', token, offset }, signal));
      const result = await extension.handle(method, { ...args, events: { kind: 'inline', value: events } }, signal);
      loading.todos += events.filter(event => event.type === 'todo/write').length;
      return result;
    }
    if (method === 'agent.load.finish') {
      if (rejectState && args.checkpoint?.schemaVersion !== 2) throw new Error('replacement requires checkpoint schema 2');
      finished++;
    }
    if (method === 'agent.tool') tools++;
    return extension.handle(method, args, signal);
  } };
}
