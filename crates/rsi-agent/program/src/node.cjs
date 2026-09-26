'use strict';
// Explicit process execution: no VM sandbox or module-isolation claim.
const write = process.stdout.write.bind(process.stdout);
const maximum = 1024 * 1024;
let buffer = Buffer.alloc(0), next = 1, started = false, finished = false;
const pending = new Map();
let maximumCalls = 0;
function send(value) {
  const body = Buffer.from(JSON.stringify(value));
  if (body.length > maximum) throw new Error('program frame exceeds 1 MiB');
  const prefix = Buffer.alloc(4); prefix.writeUInt32BE(body.length);
  write(Buffer.concat([prefix, body]));
}
function rpc(method, arguments_) {
  if (finished) return Promise.reject(new Error('program finished'));
  if (pending.size >= maximumCalls) return Promise.reject(new Error('program RPC capacity exceeded'));
  const id = next++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    try { send({type: 'call', id, method, arguments: arguments_}); }
    catch (error) { pending.delete(id); reject(error); }
  });
}
async function start(message) {
  if (started) throw new Error('duplicate program start');
  started = true;
  if (!Number.isSafeInteger(message.maximum_calls) || message.maximum_calls < 1) throw new Error('invalid RPC capacity');
  maximumCalls = message.maximum_calls;
  const tools = Object.freeze({ definitions: Object.freeze(message.definitions), call: (name, arguments_) => rpc('tool', {name, arguments: arguments_}) });
  const workflow = Object.freeze({
    agent: arguments_ => rpc('agent', arguments_),
    pipeline: async (steps, input) => { let value = input; for (const step of steps) value = await step(value); return value; },
    parallel: async (steps) => Promise.all(steps.map(step => step())),
    phase: (name, value) => rpc('phase', {name, value}),
    log: value => rpc('log', {value}),
  });
  const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
  try {
    const value = await new AsyncFunction('tools', 'workflow', 'require', message.script)(tools, workflow, require);
    if (pending.size) throw new Error('program returned with pending RPC calls');
    finished = true;
    send({type: 'result', value: value === undefined ? null : value});
  } catch (error) {
    finished = true;
    send({type: 'error', message: String(error?.message ?? error).slice(0, 4096)});
  }
  process.stdin.pause();
  process.stdin.destroy();
}
for (const name of ['log', 'info', 'warn', 'error', 'debug']) console[name] = (...values) => process.stderr.write(values.map(String).join(' ') + '\n');
process.stdin.on('data', chunk => {
  try {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4) {
      const length = buffer.readUInt32BE(0);
      if (!length || length > maximum) throw new Error('invalid program frame length');
      if (buffer.length < length + 4) break;
      const message = JSON.parse(buffer.subarray(4, length + 4).toString('utf8'));
      buffer = buffer.subarray(length + 4);
      if (message.type === 'start') void start(message).catch(error => { console.error(error); process.exitCode = 1; process.stdin.destroy(); });
      else if (message.type === 'reply') {
        const waiter = pending.get(message.id);
        if (!waiter) throw new Error('unknown RPC reply');
        pending.delete(message.id);
        if (message.error !== undefined) waiter.reject(new Error(message.error)); else waiter.resolve(message.value);
      } else throw new Error('unknown program frame');
    }
  } catch (error) { console.error(error); process.exitCode = 1; process.stdin.destroy(); }
});
