import json
import os
import sys
import time
import signal

# An explicit child with no shell, ambient environment or external services.
mode = sys.argv[1]
modern = mode.startswith('modern')
subscription = None

def send(value):
    frame = json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode() + b'\n'
    for start in range(0, len(frame), 113):
        os.write(1, frame[start:start + 113])

for raw in sys.stdin.buffer:
    request = json.loads(raw)
    method = request.get('method')
    if method is None:
        continue  # reply to the fixture's own ping
    if modern:
        assert request['params']['_meta']['io.modelcontextprotocol/protocolVersion'] == '2026-07-28'
        assert request['params']['_meta']['io.modelcontextprotocol/clientCapabilities'] == {}
        assert method not in ('initialize', 'notifications/initialized')
    if method == 'server/discover':
        if modern:
            send({'jsonrpc':'2.0','id':request['id'],'result':{'resultType':'complete','ttlMs':1000,'cacheScope':'private','supportedVersions':['2026-07-28'],'capabilities':{'tools':{'listChanged':True}},'_meta':{'io.modelcontextprotocol/serverInfo':{'name':'modern-stdio','version':'1'}}}})
        elif mode == 'legacy-silent-probe':
            with open(sys.argv[2], 'a') as marker:
                marker.write(str(os.getpid()) + '\n')
        else:
            send({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32602,'message':'initialize first'}})
        continue
    if method == 'subscriptions/listen':
        subscription = request['id']
        assert modern and request['params']['notifications']['toolsListChanged']
        send({'jsonrpc':'2.0','method':'notifications/subscriptions/acknowledged','params':{'_meta':{'io.modelcontextprotocol/subscriptionId':subscription},'notifications':{'toolsListChanged':True}}})
        continue
    if method == 'notifications/initialized':
        continue
    if method == 'initialize':
        if mode == 'legacy-silent-probe':
            with open(sys.argv[2], 'a') as marker:
                marker.write(str(os.getpid()) + '\n')
        if mode == 'initialize-stall':
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            with open(sys.argv[2], 'w') as marker:
                marker.write(str(os.getpid()))
            time.sleep(60)
        result = {'protocolVersion': '2025-11-25', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'stdio-fixture', 'version': '1'}}
    elif method == 'tools/list':
        if mode == 'oversize':
            os.write(1, b'x' * (1024 * 1024 + 1))
            time.sleep(60)
            continue
        result = {'tools': [{'name': 'echo', 'inputSchema': {'type': 'object', 'properties': {'message': {'type': 'string'}}, 'required': ['message'], 'additionalProperties': False}, 'annotations': {'readOnlyHint': True}}]}
    elif method == 'tools/call':
        os.write(2, b'stderr noise\n' * 20000)
        if mode == 'cancel-write':
            def stop_then_write(_signal, _frame):
                with open(sys.argv[2] + '.stopping', 'w') as marker: marker.write('stopping')
                while not os.path.exists(sys.argv[2] + '.release'): time.sleep(0.001)
                with open(sys.argv[2] + '.late', 'w') as marker: marker.write('settled write')
                sys.exit(0)
            signal.signal(signal.SIGTERM, stop_then_write)
            with open(sys.argv[2], 'w') as marker: marker.write(str(os.getpid()))
            time.sleep(60)
        if mode == 'stall':
            with open(sys.argv[2], 'w') as marker:
                marker.write(str(os.getpid()))
            time.sleep(60)
        result = {'content': [{'type': 'text', 'text': request['params']['arguments']['message']}]}
    else:
        raise RuntimeError('Unexpected client method')
    if modern:
        result.update(resultType='complete', ttlMs=1000, cacheScope='private')
    send({'jsonrpc': '2.0', 'id': request['id'], 'result': result})
    if method == 'tools/list' and mode == 'half-close':
        # An explicit FIFO gates the close until the client has published readiness.
        with open(sys.argv[2] + '.close', 'rb', buffering=0) as gate:
            gate.read(1)
        os.close(1)
        with open(sys.argv[2], 'w') as marker:
            marker.write(str(os.getpid()))
        sys.stdin.buffer.read()
        break
    if method == 'tools/call' and mode == 'modern-changed':
        send({'jsonrpc':'2.0','method':'notifications/tools/list_changed','params':{'_meta':{'io.modelcontextprotocol/subscriptionId':subscription}}})
    if method == 'tools/call' and mode == 'changed':
        send({'jsonrpc': '2.0', 'method': 'notifications/tools/list_changed'})
    if method == 'tools/list' and mode == 'duplex-cycle':
        # Wait until the client starts a frame larger than the stdin pipe, then
        # fill stdout after a server request. Reading must continue while its
        # reply waits for the frame writer.
        first = sys.stdin.buffer.read(1)
        send({'jsonrpc': '2.0', 'id': 'server-ping', 'method': 'ping'})
        send({'jsonrpc': '2.0', 'method': 'notifications/progress', 'params': {'text': 'x' * (256 * 1024)}})
        call = json.loads(first + sys.stdin.buffer.readline())
        send({'jsonrpc': '2.0', 'id': call['id'], 'result': {'content': [{'type': 'text', 'text': call['params']['arguments']['message']}]}})
