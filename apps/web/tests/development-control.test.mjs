import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough} from 'node:stream';
import {DevelopmentControl} from '../development-control.mjs';
import {EventEmitter} from 'node:events';
import {runDevelopmentServer} from '../development-server.mjs';
import {readFileSync} from 'node:fs';
import {stat} from 'node:fs/promises';
import {createServer} from 'vite';
import {decodeFrame} from '../development-control.mjs';
import configuration from '../vite.config.mjs';

test('invalid control input always reports the protocol error', () => {
  for (const message of [
    ...['constructor','toString','__proto__'].map(event => ({event})),
    {event:'frontend_bound',origin:'not-a-url'},
  ]) {
    const bytes = Buffer.from(JSON.stringify({version:1,run_id:'a'.repeat(32),message})+'\n');
    assert.throws(() => decodeFrame(bytes), {message:'Invalid development control frame'});
  }
  for (const bytes of [Buffer.from('{broken\n'), Buffer.from([0xff,10])]) {
    assert.throws(() => decodeFrame(bytes), {message:'Invalid development control frame'});
  }
});

test('an environment flag cannot start an unsupervised source server', () => {
  const previous = {upstream:process.env.RSI_DEV_UPSTREAM,control:process.env.RSI_DEV_CONTROL};
  try {
    delete process.env.RSI_DEV_UPSTREAM;
    process.env.RSI_DEV_CONTROL = '1';
    assert.throws(() => configuration({command:'serve'}), /Start product development/);
  } finally {
    for (const [key,value] of [['RSI_DEV_UPSTREAM',previous.upstream],['RSI_DEV_CONTROL',previous.control]]) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
  }
});

test('the frontend decoder agrees with the shared Rust corpus', () => {
  const corpus = JSON.parse(readFileSync(new URL('../../dev-control/conformance.json', import.meta.url), 'utf8'));
  for (const sample of corpus.cases) {
    let accepted = true;
    try { decodeFrame(Buffer.from(sample.json+'\n')); } catch { accepted = false; }
    assert.equal(accepted, sample.valid, sample.name);
  }
});

test('partial-frame EOF fails the control session', async () => {
  const input = new PassThrough(), output = new PassThrough();
  let reason;
  const control = new DevelopmentControl(input, output, error => { reason = error; });
  const waiting = assert.rejects(control.expect('start_frontend'));
  input.end('{"version":1');
  await waiting;
  assert(reason instanceof Error, 'partial EOF must be a protocol failure');
});

test('SIGINT returns an interrupt status while waiting for startup', async () => {
  const input = new PassThrough(), output = new PassThrough();
  const running = runDevelopmentServer({input, output});
  process.emit('SIGINT');
  assert.equal(await running, 130);
});

test('control closure during listener startup closes the eventual listener', async () => {
  const input = new PassThrough(), output = new PassThrough();
  const socket = new EventEmitter();
  let entered;
  const listening = new Promise(resolve => { entered = resolve; });
  socket.listen = () => { entered(); };
  socket.address = () => ({address:'127.0.0.1',port:43210});
  socket.listening = false;
  const server = {httpServer:socket,middlewares:{use() {}},close:async () => { socket.listening = false; }};
  const running = runDevelopmentServer({input,output,createServer:async config => {
    config.plugins[0].configureServer(server);
    return server;
  }});
  input.write(JSON.stringify({version:1,run_id:'a'.repeat(32),message:{event:'start_frontend',port:0,family:'b'.repeat(64)}})+'\n');
  await listening;
  input.end();
  await new Promise(resolve => setImmediate(resolve));
  socket.listening = true;
  socket.emit('listening');
  await running;
  assert.equal(socket.listening, false, 'a listener must not survive a completed stop');
});

test('real Vite EOF settlement removes only its private optimizer cache', {timeout:25000}, async () => {
  const input = new PassThrough(), output = new PassThrough();
  const send = message => input.write(JSON.stringify({version:1,run_id:'a'.repeat(32),message})+'\n');
  let directory;
  output.on('data', bytes => {
    const {message} = decodeFrame(bytes);
    if (message.event === 'frontend_bound') send({event:'configure_proxy',upstream:'http://127.0.0.1:12345'});
    else if (message.event === 'proxy_ready') input.end();
  });
  const running = runDevelopmentServer({input,output,createServer:async config => {
    directory = config.cacheDir;
    return createServer(config);
  }});
  send({event:'start_frontend',port:0,family:'b'.repeat(64)});
  assert.equal(await running, 0);
  await assert.rejects(stat(directory), {code:'ENOENT'});
});
