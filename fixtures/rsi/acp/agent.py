import json
import os
import signal
import sys
import time

workspace, mode = sys.argv[1:]
assert os.environ.get('FIXTURE_SECRET') == 'private-fixture-secret'
with open(os.path.join(workspace, 'peer-pids'), 'a', encoding='utf-8') as out:
    out.write(str(os.getpid()) + '\n')
if mode == 'stall':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

pending = None
permission_prompt = None

def reply(identity, value):
    print(json.dumps({'jsonrpc': '2.0', 'id': identity, 'result': value}), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method, params = message.get('method'), message.get('params', {})
    identity = message.get('id')
    if method == 'initialize':
        if mode == 'slow-phases': time.sleep(8)
        if mode == 'stall':
            continue
        reply(identity, {'protocolVersion': 1, 'agentCapabilities': {'loadSession': True, 'sessionCapabilities': {'resume': {}, 'close': {}}}})
    elif method in ('session/new', 'session/resume', 'session/load'):
        if mode == 'slow-phases': time.sleep(27)
        assert params['cwd'] == workspace
        assert params['mcpServers'][0]['env'][0]['value'] == 'private-fixture-secret'
        if method == 'session/new':
            with open(os.path.join(workspace, 'new-count'), 'a', encoding='utf-8') as out:
                out.write('new\n')
        if method == 'session/load':
            print(json.dumps({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 'fixture-remote', 'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'replayed'}}}}), flush=True)
        result = {'sessionId': 'fixture-remote'} if method == 'session/new' else {}
        if mode == 'slow-phases': result['configOptions'] = [{'id':'mode','name':'Mode','type':'select','currentValue':'default','options':[{'value':'default','name':'Default'},{'value':'selected','name':'Selected'}]}]
        reply(identity, result)
    elif method == 'session/set_config_option':
        assert mode == 'slow-phases' and params['configId'] == 'mode' and params['value'] == 'selected'
        time.sleep(12)
        reply(identity, {'configOptions':[{'id':'mode','name':'Mode','type':'select','currentValue':'selected','options':[{'value':'default','name':'Default'},{'value':'selected','name':'Selected'}]}]})
    elif method == 'session/prompt':
        pending = identity
        if params['prompt'][0]['text'] == 'permission':
            permission_prompt = identity
            print(json.dumps({'jsonrpc': '2.0', 'id': 'exact-permission', 'method': 'session/request_permission', 'params': {'sessionId': 'fixture-remote', 'toolCall': {'toolCallId': 'fixture-tool', 'title': 'Read fixture', 'status': 'pending'}, 'options': [{'optionId': kind, 'name': kind, 'kind': kind} for kind in ['allow_once', 'allow_always', 'reject_once', 'reject_always']]}}), flush=True)
            continue
        if params['prompt'][0]['text'] == 'wait':
            continue
        print(json.dumps({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 'fixture-remote', 'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'fixture-output'}}}}), flush=True)
        reply(identity, {'stopReason': 'end_turn'})
        pending = None
    elif method == 'session/cancel':
        if mode == 'slow-phases': time.sleep(20)
        if pending is not None:
            reply(pending, {'stopReason': 'cancelled'})
            pending = None
    elif method == 'session/close':
        if mode == 'slow-phases': time.sleep(20)
        reply(identity, {})
    elif identity == 'exact-permission':
        assert message['result']['outcome']['optionId'] in ['allow_once', 'allow_always', 'reject_once', 'reject_always']
        print(json.dumps({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 'fixture-remote', 'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'fixture-permission-answered 中文'}}}}), flush=True)
        reply(permission_prompt, {'stopReason': 'end_turn'})
        permission_prompt = None
        pending = None
    elif identity is not None:
        raise RuntimeError('unexpected request')
