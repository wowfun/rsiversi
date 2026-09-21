import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile, mkdtemp, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { Readable, Writable } from 'node:stream';
import { parseArgs } from 'node:util';
import { ClientSideConnection, ndJsonStream } from '@agentclientprotocol/sdk';

const { values } = parseArgs({ options: {
  binary: { type: 'string', default: 'target/debug/rsi' },
  output: { type: 'string' }, 'live-env': { type: 'string' },
} });
assert(values.output, '--output is required');
const output = resolve(values.output);
await mkdir(output, { recursive: true });
// Keep the isolated Unix socket path below sockaddr_un's native byte limit.
const root = await mkdtemp(join(tmpdir(), 'rsi-'));
const workspace = join(root, 'workspace');
const config = join(root, 'config/rsi');
await Promise.all([workspace, config, join(root, 'state'), join(root, 'cache'), join(root, 'home'), join(root, 'runtime')].map(path => mkdir(path, { recursive: true })));
await chmod(join(root, 'runtime'), 0o700);
let credential = 'fixture-secret';
let endpoint;
let model = 'fixture-model';
let server;
let requests = 0;
let child;
const live = !!values['live-env'];
if (live) {
  const source = await readFile(resolve(values['live-env']), 'utf8');
  const match = source.match(/^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$/m);
  assert(match, 'DEEPSEEK_API_KEY unavailable');
  credential = match[1].replace(/^(['"])(.*)\1$/, '$2');
  assert(credential.length > 0, 'DEEPSEEK_API_KEY empty');
  endpoint = 'https://api.deepseek.com';
  model = 'deepseek-flash';
} else {
  server = createServer(async (request, response) => {
    for await (const chunk of request) { void chunk; }
    requests += 1;
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    for (let index = 0; index < 1200; index += 1) {
      response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: `part-${index}\n` }, finish_reason: null }] })}\n\n`);
    }
    response.end(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 2, completion_tokens: 1200 } })}\n\ndata: [DONE]\n\n`);
  }).listen(0, '127.0.0.1');
  await once(server, 'listening');
  endpoint = `http://127.0.0.1:${server.address().port}`;
}
await writeFile(join(config, 'settings.json'), JSON.stringify({ 'rsi.agent': { default_model: { deployment: 'fixture', model }, require_approval: true } }));
await mkdir(join(config, 'host-profiles/fixture'), { recursive: true });
await mkdir(join(config, 'application-profiles/test-acp'), { recursive: true });
await writeFile(join(config, 'host-profiles/fixture/host.profile.toml'), `format = 1
[[steps]]
kind = "plugin"
id = "fixture-provider"
plugin = "rsi.ai.provider.${live ? 'deepseek' : 'openai-compatible'}"
[steps.config]
deployment = "fixture"
endpoint = "${endpoint}"
${live ? 'protocol = "chat-completions"' : 'path = "/v1/chat/completions"\nallow_image_input = false'}
credential = { owner = "rsi.ai.provider.openai-compatible", slot = "default" }
[steps.config.language_models.${model}]
context_window_tokens = 128000
default_output_reserve_tokens = 4096
max_output_reserve_tokens = 16384
`);
await writeFile(join(config, 'application-profiles/test-acp/application.profile.toml'), `format = 1
[[steps]]
kind = "plugin"
id = "service"
plugin = "rsi.application.acp-service"
config = { host_profile = "fixture" }
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.acp"
`);
const environment = { ...process.env, HOME: join(root, 'home'), XDG_CONFIG_HOME: join(root, 'config'), XDG_STATE_HOME: join(root, 'state'), XDG_CACHE_HOME: join(root, 'cache'), XDG_RUNTIME_DIR: join(root, 'runtime'), RSI_OPENAI_COMPATIBLE_API_KEY: credential };
for (const key of ['DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'RSI_DEEPSEEK_API_KEY', 'RSI_OPENAI_API_KEY']) delete environment[key];
const report = { sdk: '@agentclientprotocol/sdk@1.4.0', mode: live ? 'live' : 'deterministic', root, model, ok: false };
let stderr = '';
let phase = 'startup';
const deadline = setTimeout(() => { child?.kill('SIGTERM'); }, live ? 180_000 : 90_000);
const started = Date.now();
try {
  child = spawn(resolve(values.binary), ['--profile', 'test-acp'], { cwd: workspace, env: environment, stdio: ['pipe', 'pipe', 'pipe'] });
  const exit = once(child, 'exit');
  child.stderr.on('data', chunk => { if (stderr.length < 65536) stderr += chunk.toString(); });
  const updates = [];
  let approvals = 0;
  const peer = new ClientSideConnection(() => ({
    sessionUpdate: async params => { updates.push(params); },
    requestPermission: async params => {
      approvals += 1;
      const option = params.options.find(option => option.kind === 'allow_once');
      assert(option);
      return { outcome: { outcome: 'selected', optionId: option.optionId } };
    },
  }), ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout)));
  phase = 'initialize';
  const initialized = await peer.initialize({ protocolVersion: 1, clientCapabilities: {} });
  assert.equal(initialized.protocolVersion, 1);
  assert.equal(initialized.agentCapabilities.loadSession, true);
  phase = 'new';
  const { sessionId } = await peer.newSession({ cwd: workspace, mcpServers: [] });
  const prompt = live ? 'Use bash to write exactly acp-live-ok followed by a newline to acp-live.txt in the current workspace, then use bash to read the file. Finish with ACP_LIVE_VERIFIED after checking the output. Do not modify any other files.' : 'ancient SDK input';
  phase = 'prompt';
  const result = await peer.prompt({ sessionId, prompt: [{ type: 'text', text: prompt }] });
  assert.equal(result.stopReason, 'end_turn');
  const observed = updates.slice();
  const assistantText = observed.filter(event => event.update.sessionUpdate === 'agent_message_chunk').map(event => event.update.content.text ?? '').join('');
  if (live) {
    assert.equal(await readFile(join(workspace, 'acp-live.txt'), 'utf8'), 'acp-live-ok\n');
    assert(assistantText.includes('ACP_LIVE_VERIFIED'));
  } else {
    assert.equal(observed.length, 1200);
    assert.equal(requests, 1);
  }
  updates.length = 0;
  phase = 'load';
  await peer.loadSession({ sessionId, cwd: workspace, mcpServers: [] });
  assert(updates.some(event => event.update.sessionUpdate === 'user_message_chunk' && event.update.content.text === prompt));
  const replayText = updates.filter(event => event.update.sessionUpdate === 'agent_message_chunk').map(event => event.update.content.text ?? '').join('');
  assert.equal(replayText, assistantText);
  if (!live) assert.equal(updates.length, 1201);
  report.prompt_updates = observed.length;
  report.replay_updates = updates.length;
  phase = 'resume';
  updates.length = 0;
  await peer.resumeSession({ sessionId, cwd: workspace, mcpServers: [] });
  const list = await peer.listSessions({});
  assert(list.sessions.some(session => session.sessionId === sessionId));
  assert.equal(updates.length, 0);
  phase = 'close';
  await peer.closeSession({ sessionId });
  child.stdin.end();
  const [code, signal] = await exit;
  assert.equal(code, 0);
  assert.equal(signal, null);
  report.ok = true;
  report.approvals = approvals;
  report.mock_requests = requests;
  report.exit_code = code;
} catch (error) {
  report.failed_phase = phase;
  report.error = String(error.message).replaceAll(credential, '[REDACTED]');
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  if (child?.exitCode === null && child?.signalCode === null) {
    const stopped = once(child, 'exit');
    child.kill('SIGTERM');
    const force = setTimeout(() => child.kill('SIGKILL'), 5_000);
    try { await stopped; } finally { clearTimeout(force); }
  }
  server?.closeAllConnections();
  if (server) await new Promise(resolve => server.close(resolve));
  report.elapsed_ms = Date.now() - started;
  await writeFile(join(output, 'stderr.log'), stderr.replaceAll(credential, '[REDACTED]'));
  await writeFile(join(output, 'result.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report));
}
