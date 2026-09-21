import assert from 'node:assert/strict';
import { appendFileSync } from 'node:fs';
import { Readable, Writable } from 'node:stream';
import { AgentSideConnection, ndJsonStream, PROTOCOL_VERSION } from '@agentclientprotocol/sdk';

const workspace = process.argv[2];
assert.equal(process.env.FIXTURE_SECRET, 'private-fixture-secret');
appendFileSync(`${workspace}/peer-pids`, `${process.pid}\n`);
let release;
const checkSetup = (params) => {
  assert.equal(params.cwd, workspace);
  assert.equal(params.mcpServers.length, 1);
  assert.equal(params.mcpServers[0].env[0].value, 'private-fixture-secret');
};
const connection = new AgentSideConnection(peer => ({
  async initialize() {
    return { protocolVersion: PROTOCOL_VERSION, agentCapabilities: { loadSession: true, sessionCapabilities: { resume: {}, close: {} } } };
  },
  async newSession(params) {
    checkSetup(params);
    appendFileSync(`${workspace}/new-count`, 'new\n');
    return { sessionId: 'sdk-remote' };
  },
  async resumeSession(params) { checkSetup(params); assert.equal(params.sessionId, 'sdk-remote'); return {}; },
  async loadSession(params) {
    checkSetup(params);
    for (let index = 0; index < 1200; index++) {
      await peer.sessionUpdate({ sessionId: 'sdk-remote', update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: String(index) } } });
    }
    return {};
  },
  async prompt(params) {
    assert.equal(params.sessionId, 'sdk-remote');
    if (params.prompt[0].text === 'wait') return new Promise(resolve => { release = () => resolve({ stopReason: 'cancelled' }); });
    const decision = await peer.requestPermission({ sessionId: params.sessionId, toolCall: { toolCallId: 'sdk-tool', title: 'Independent SDK permission' }, options: [
      { optionId: 'sdk-exact-once', name: 'Once', kind: 'allow_once' },
      { optionId: 'sdk-exact-always', name: 'Always', kind: 'allow_always' },
      { optionId: 'sdk-exact-reject', name: 'Reject', kind: 'reject_once' },
      { optionId: 'sdk-exact-never', name: 'Never', kind: 'reject_always' },
    ] });
    assert.equal(decision.outcome.outcome, 'selected');
    assert.equal(decision.outcome.optionId, params.prompt[0].text === 'reject-always' ? 'sdk-exact-never' : 'sdk-exact-always');
    await peer.sessionUpdate({ sessionId: params.sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'SDK_AGENT_VERIFIED 中文 <script>window.externalExecuted=true</script>' } } });
    return { stopReason: 'end_turn' };
  },
  async cancel() { release?.(); release = undefined; },
  async closeSession() { return {}; },
}), ndJsonStream(Writable.toWeb(process.stdout), Readable.toWeb(process.stdin)));
await connection.closed;
