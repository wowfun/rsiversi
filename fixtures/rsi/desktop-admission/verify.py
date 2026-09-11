"""Exercise the real Tauri window through WebKitGTK WebDriver under Xvfb."""
import argparse
import base64
import http.client
import json
from pathlib import Path
import os
import socket
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--driver', type=Path, required=True)
parser.add_argument('--report', type=Path, required=True)
args = parser.parse_args()
args.report.mkdir(parents=True, exist_ok=False)
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
env = os.environ.copy()
env.update(TAURI_WEBVIEW_AUTOMATION='true', RSI_ADMISSION_AUTOMATION='true',
           RSI_ADMISSION_REPORT=str((args.report/'native.json').resolve()))
for key in ('XDG_CACHE_HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_RUNTIME_DIR'):
    path = args.report/key.lower(); path.mkdir(mode=0o700); env[key] = str(path.resolve())
log = (args.report/'webdriver.log').open('w')
process = subprocess.Popen([str(args.driver.resolve()), '--host=127.0.0.1', f'--port={port}'], env=env, stdout=log, stderr=subprocess.STDOUT)

def call(method, path, body=None):
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=20)
    try:
        connection.request(method, path, None if body is None else json.dumps(body), {'Content-Type':'application/json'})
        response = connection.getresponse()
        result = json.loads(response.read(32 * 1024 * 1024))
        if response.status >= 400: raise RuntimeError(result)
        return result.get('value')
    finally: connection.close()

def until(read, timeout=30):
    deadline = time.monotonic() + timeout
    while True:
        value = read()
        if value: return value
        if time.monotonic() >= deadline: raise TimeoutError('WebDriver condition deadline')
        time.sleep(0.02)

session = None
try:
    def ready():
        try: return call('GET', '/status')
        except (OSError, http.client.HTTPException): return None
    until(ready)
    result = call('POST', '/session', {'capabilities':{'alwaysMatch':{
        'webkitgtk:browserOptions':{'binary':str(args.binary.resolve())}
    }}})
    session = result['sessionId']
    root = f'/session/{session}'
    def script(source, arguments=None): return call('POST', root+'/execute/sync', {'script':source, 'args':arguments or []})
    report = until(lambda: script('return window.admissionReport || null'))
    assert report['ok'], report
    script('window.paintSamples=[]; document.querySelector("textarea").addEventListener("input",()=>{const start=performance.now();requestAnimationFrame(()=>window.paintSamples.push(performance.now()-start));});')
    element = call('POST', root+'/element', {'using':'css selector','value':'textarea'})
    element_id = next(iter(element.values()))
    call('POST', root+f'/element/{element_id}/value', {'text':' WebDriver 中文', 'value':list(' WebDriver 中文')})
    until(lambda: script('return window.paintSamples.length > 0'))
    assert 'WebDriver 中文' in script('return document.querySelector("textarea").value')
    (args.report/'window.png').write_bytes(base64.b64decode(call('GET', root+'/screenshot'), validate=True))
    report['automation'] = {'capabilities':result['capabilities'], 'inputToFrameMs':script('return window.paintSamples'), 'actualTyping':True}
    (args.report/'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    script('window.__TAURI__.core.invoke("request_exit"); return true;')
    until(lambda: 'admission: main-thread exit after drain' in (args.report/'webdriver.log').read_text())
    report['automation']['mainThreadExitAfterDrain'] = True
    (args.report/'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps(report, ensure_ascii=False))
finally:
    if session:
        try: call('DELETE', f'/session/{session}')
        except (OSError, RuntimeError, http.client.HTTPException): pass
    process.terminate()
    try: process.wait(timeout=10)
    except subprocess.TimeoutExpired: process.kill(); process.wait()
    log.close()
