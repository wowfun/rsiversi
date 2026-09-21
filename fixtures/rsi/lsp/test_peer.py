"""Bounded fixture for serialized LSP lifecycle and adversarial wire tests."""
import json, os, sys
mode = os.environ.get('LSP_TEST_MODE', 'normal')
log_path = os.environ['LSP_TEST_LOG']
documents = {}
pending = None

def log(event, **data):
    with open(log_path, 'a', encoding='utf-8') as f: f.write(json.dumps({'event':event,'pid':os.getpid(),**data},ensure_ascii=False)+'\n')

def send(value):
    body = json.dumps({'jsonrpc':'2.0',**value},ensure_ascii=False).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode()+body);sys.stdout.buffer.flush()

log('start')
while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline(8193)
        if not line: sys.exit(0)
        if line == b'\r\n': break
        k,v = line.decode('ascii').strip().split(':',1);headers[k.lower()]=v.strip()
    length = int(headers['content-length'])
    assert 0 < length <= 2 * 1024 * 1024
    message = json.loads(sys.stdin.buffer.read(length))
    method = message.get('method')
    if method == 'initialize':
        send({'id':message['id'],'result':{'capabilities':{'positionEncoding':'utf-8' if mode=='encoding' else 'utf-16','textDocumentSync':{'openClose':True,'change':1},'definitionProvider':True,'referencesProvider':True,'implementationProvider':True,'hoverProvider':True}}})
    elif method == 'shutdown': send({'id':message['id'],'result':None})
    elif method == 'exit': log('exit');sys.exit(0)
    elif method == 'textDocument/didOpen':
        doc = message['params']['textDocument'];documents[doc['uri']] = doc['text'];log('open',text=doc['text'],version=doc['version'])
    elif method == 'textDocument/didClose':
        uri = message['params']['textDocument']['uri'];documents.pop(uri);log('close')
    elif method == '$/cancelRequest':
        log('cancel',id=message['params']['id']);send({'id':message['params']['id'],'error':{'code':-32800,'message':'cancelled'}})
    elif method and method.startswith('textDocument/'):
        log('query',method=method,params=message['params'])
        if mode == 'hang': continue
        if mode == 'exit': sys.exit(7)
        if mode == 'server-error-closed-input':
            os.close(0)
            send({'id':message['id'],'error':{'code':-32801,'message':'SECRET SERVER DETAIL'}})
            sys.exit(0)
        if mode == 'server-error':
            send({'id':message['id'],'error':{'code':-32801,'message':'SECRET SERVER DETAIL','data':'SECRET DATA'}});continue
        if mode == 'oversize':
            sys.stdout.buffer.write(b'Content-Length: 100000000\r\n\r\n');sys.stdout.buffer.flush();continue
        if mode == 'notifications':
            for _ in range(260): send({'method':'window/logMessage','params':{'type':4,'message':'x'*16384}})
            continue
        if mode == 'indexing':
            for _ in range(1000): send({'method':'$/progress','params':{'token':'index','value':{'kind':'report','percentage':50}}})
        uri = message['params']['textDocument']['uri']
        result = {'contents':{'kind':'plaintext','value': 'h' * 16385 if mode=='hover-limit' else 'fixture hover'}} if method=='textDocument/hover' else [{'uri':'file:///outside/secret' if mode=='escape' else uri,'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':2}}}]
        if mode == 'edit':
            pending = {'id':message['id'],'result':result};send({'id':'edit','method':'workspace/applyEdit','params':{'edit':{'changes':{uri:[]}}}})
        else: send({'id':message['id']+1 if mode=='wrong-id' else message['id'],'result':result})
    elif message.get('id') == 'edit':
        assert message.get('error',{}).get('code') == -32601;log('edit_rejected');send(pending);pending=None
