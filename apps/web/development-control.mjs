// Child-pipe protocol counterpart of rsi-dev-control. Never log frame payloads.
import { TextDecoder } from 'node:util';
import {readFileSync} from 'node:fs';
import {validProtocolIdentity, wireDescriptor} from '../devtools/wire-descriptors.mjs';
export const protocol = wireDescriptor(new URL('../dev-control/wire-contract.json', import.meta.url), 'rsi.frontend.control');
const limits = JSON.parse(readFileSync(new URL('../dev-control/conformance.json', import.meta.url), 'utf8'));
const maximum = limits.maximum_frame_bytes;
const startupTimeout = limits.startup_timeout_ms;
const schemas = {
  start_frontend: ['port', 'family'], frontend_bound: ['origin'],
  configure_proxy: ['upstream'], proxy_ready: [],
};
const failure = () => new Error('Invalid development control frame');
export function loopbackAddress(origin) {
  if (typeof origin !== 'string') throw failure();
  let url;
  try { url = new URL(origin); } catch { throw failure(); }
  if (url.origin !== origin || url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || url.username || url.password || url.pathname !== '/' || url.search || url.hash || url.port === '0') throw failure();
  return {host: '127.0.0.1', port: Number(url.port || 80)};
}
function keys(object, expected) {
  if (!object || typeof object !== 'object' || Array.isArray(object) || Object.keys(object).sort().join(',') !== [...expected].sort().join(',')) throw failure();
}
export function decodeFrame(bytes) {
  try { return parseFrame(bytes); } catch { throw failure(); }
}
function parseFrame(bytes) {
  if (bytes.length > maximum || bytes.at(-1) !== 10) throw failure();
  const text = new TextDecoder('utf-8', {fatal: true}).decode(bytes);
  const frame = JSON.parse(text);
  // JSON.parse alone silently accepts duplicate object keys. Parse the validated
  // string tokens again to reject duplicates before accepting any authority.
  const stack = [];
  for (const token of text.matchAll(/"(?:[^"\\]|\\.)*"|[{}\[\]]/g)) {
    const value = token[0];
    if (value === '{') stack.push(new Set());
    else if (value === '[') stack.push(null);
    else if (value === '}' || value === ']') stack.pop();
    else if (text.slice(token.index + value.length).trimStart().startsWith(':')) {
      const key = JSON.parse(value), current = stack.at(-1);
      if (!current || current.has(key)) throw failure();
      if (['version','port'].includes(key) && !/^:\s*(?:0|[1-9]\d*)(?=\s*[,}])/.test(text.slice(token.index + value.length).trimStart())) throw failure();
      current.add(key);
    }
  }
  keys(frame, ['version', 'interface', 'run_id', 'message']);
  keys(frame.interface, ['name', 'version']);
  if (frame.interface.name !== protocol.name || frame.interface.version !== protocol.version) throw failure();
  if (frame.version !== 1 || typeof frame.run_id !== 'string' || !/^[a-f0-9]{32}$/.test(frame.run_id)) throw failure();
  const message = frame.message;
  if (typeof message?.event !== 'string' || !Object.hasOwn(schemas, message.event)) throw failure();
  const fields = schemas[message.event];
  keys(message, ['event', ...fields]);
  if (message.event === 'start_frontend' && (!Number.isInteger(message.port) || message.port < 0 || message.port > 65535 || typeof message.family !== 'string' || !/^[a-f0-9]{64}$/.test(message.family))) throw failure();
  if (message.event === 'frontend_bound') loopbackAddress(message.origin);
  if (message.event === 'configure_proxy') loopbackAddress(message.upstream);
  return frame;
}
export class DevelopmentControl {
  #input; #output; #buffer = Buffer.alloc(0); #waiting; #run; #closed = false; #onStop;
  constructor(input, output, onStop) {
    this.#input = input; this.#output = output; this.#onStop = onStop;
    input.on('data', chunk => {
      try {
        let offset = 0;
        while (offset < chunk.length) {
          const newline = chunk.indexOf(10, offset);
          const end = newline === -1 ? chunk.length : newline + 1;
          if (this.#buffer.length + end - offset > maximum) throw failure();
          this.#buffer = Buffer.concat([this.#buffer, chunk.subarray(offset, end)]);
          offset = end;
          if (newline === -1) break;
          const frame = decodeFrame(this.#buffer);
          this.#buffer = Buffer.alloc(0);
          if (this.#run && frame.run_id !== this.#run) throw failure();
          this.#run ??= frame.run_id;
          if (!this.#waiting || this.#waiting.event !== frame.message.event) throw failure();
          const waiting = this.#waiting; this.#waiting = undefined; clearTimeout(waiting.timer); waiting.resolve(frame.message);
        }
        if (this.#buffer.length >= maximum) throw failure();
      } catch { this.close(failure()); }
    });
    input.on('end', () => this.close(this.#buffer.length ? failure() : undefined)); input.on('error', () => this.close(failure()));
    output.on('error', () => this.close(failure()));
  }
  expect(event, timeout = startupTimeout) {
    if (this.#closed || this.#waiting) return Promise.reject(failure());
    return new Promise((resolve, reject) => {
      const timer = timeout === null ? undefined : setTimeout(() => this.close(new Error('Development readiness timeout')), timeout);
      this.#waiting = {event, resolve, reject, timer};
    });
  }
  async send(message) {
    if (this.#closed) throw failure();
    const bytes = Buffer.from(JSON.stringify({version: 1, interface:protocol, run_id: this.#run, message}) + '\n');
    decodeFrame(bytes);
    await new Promise((resolve, reject) => this.#output.write(bytes, error => error ? reject(failure()) : resolve()));
  }
  close(error) {
    if (this.#closed) return;
    this.#closed = true;
    if (this.#waiting) { clearTimeout(this.#waiting.timer); this.#waiting.reject(error ?? new Error('Development control closed')); this.#waiting = undefined; }
    this.#onStop(error);
  }
}
