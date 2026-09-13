"""Actual Linux Tauri/WebKitGTK first-conversation and cleanup evidence."""
import argparse
import base64
import http.client
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import re
from pathlib import Path
import socket
import shutil
import subprocess
from diagnostics import record_failure, redact_evidence, run_cleanup
import sys
import threading
import tempfile
import time
from tasks import ProviderControl, verify as verify_tasks

parser = argparse.ArgumentParser(description=__doc__)
for option in ('binary', 'driver', 'assets', 'report'):
    parser.add_argument('--' + option, type=Path, required=True)
parser.add_argument('--live-env-file', type=Path)
parser.add_argument('--live-model')
parser.add_argument('--window-close', action='store_true')
parser.add_argument('--restart', action='store_true')
parser.add_argument('--daemon', action='store_true')
parser.add_argument('--foreign-binary', type=Path)
parser.add_argument('--ack-timeout', action='store_true')
parser.add_argument('--save-failure', action='store_true')
parser.add_argument('--startup-close', action='store_true')
parser.add_argument('--refresh-during-click', action='store_true')
parser.add_argument('--tasks', action='store_true')
args = parser.parse_args()
if bool(args.live_env_file) != bool(args.live_model):
    parser.error('live mode requires both an authorized environment file and a model')
if args.tasks and (args.live_env_file or args.startup_close or args.ack_timeout):
    parser.error('task mechanisms require the ordinary deterministic product scenario')
if args.foreign_binary and not args.daemon: parser.error('foreign build check requires --daemon')
if args.ack_timeout and args.restart: parser.error('ACK timeout and clean restart are distinct scenarios')
if args.startup_close and (args.restart or args.save_failure or args.ack_timeout or args.live_env_file or args.refresh_during_click):
    parser.error('startup close is a separate deterministic scenario')
secret = None
if args.live_env_file:
    source = args.live_env_file.read_bytes()
    if len(source) > 65536: raise ValueError('live environment exceeds its bound')
    match = re.search(r'^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$', source.decode(), re.M)
    if not match: raise ValueError('authorized DeepSeek key is absent')
    secret = re.sub(r'''^(["'])(.*)\1$''', r'\2', match[1].strip())
    if not secret: raise ValueError('authorized DeepSeek key is empty')
args.report.mkdir(parents=True, exist_ok=False)
if args.startup_close:
    stalled_assets = args.report / 'stalled-assets'
    shutil.copytree(args.assets, stalled_assets)
    bootstrap = stalled_assets / 'app.js'
    original = bootstrap.read_bytes()
    bootstrap.write_bytes(b'window.fixtureStartupStalled=true;await new Promise(()=>{});\n' + original)
    (args.report / 'startup-instrumentation.json').write_text(json.dumps({'originalAppSha256': hashlib.sha256(original).hexdigest(), 'stalledAppSha256': hashlib.sha256(bootstrap.read_bytes()).hexdigest(), 'boundary': 'isolated document module is stalled before startup; production Rust binary is unchanged'}, indent=2))
    args.assets = stalled_assets
requests = []
task_provider = ProviderControl()
class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 4 * 1024 * 1024:
            self.send_error(413); return
        body = json.loads(self.rfile.read(size))
        requests.append({'model': body.get('model'), 'messages': len(body.get('messages', []))})
        if args.tasks and task_provider.handle(self, body):
            return
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
        delta = {'choices': [{'delta': {'role': 'assistant', 'content': 'Desktop conversation verified. 中文输入已收到。'}, 'finish_reason': None}]}
        done = {'choices': [{'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 12}}
        self.wfile.write(('data: ' + json.dumps(delta) + '\n\ndata: ' + json.dumps(done) + '\n\ndata: [DONE]\n\n').encode())
provider = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
env = {key: value for key, value in os.environ.items() if key in ('PATH', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'LANG', 'XAUTHORITY')}
env.update(TAURI_WEBVIEW_AUTOMATION='true', RSI_OPENAI_COMPATIBLE_API_KEY='isolated-desktop-fixture')
if secret: env['DEEPSEEK_API_KEY'] = secret
for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'XDG_RUNTIME_DIR'):
    path = args.report / key.lower(); path.mkdir(mode=0o700); env[key] = str(path.resolve())
runtime_directory = tempfile.TemporaryDirectory(prefix='rsi-desktop-', dir='/tmp')
env['XDG_RUNTIME_DIR'] = runtime_directory.name
config = Path(env['XDG_CONFIG_HOME']) / 'rsi'
host = config / 'host-profiles/fixture'; host.mkdir(parents=True)
(host / 'host.profile.toml').write_text('format = 1\nsteps = []\n')
(config / 'settings.json').write_text('{"rsi.agent":{}}')
workspace = args.report / 'workspace'; workspace.mkdir()
daemon = None
daemon_log = None
daemon_metadata = None
companion = args.binary.resolve().with_name('rsi')
if args.daemon:
    receipt = json.loads(companion.with_name('receipt.json').read_text())
    assert hashlib.sha256(companion.with_name('build-family.json').read_bytes()).hexdigest() == receipt['family_sha256']
    for name in ('rsi', 'rsi-desktop'):
        assert hashlib.sha256(companion.with_name(name).read_bytes()).hexdigest() == receipt['artifacts'][name]
    libraries = subprocess.check_output(['ldd', str(companion)], text=True)
    assert not re.search(r'lib(?:gtk|webkit|javascriptcore)', libraries, re.I), libraries
    (args.report / 'headless-libraries.txt').write_text(libraries)
    daemon_log = (args.report / 'daemon.log').open('w')
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
log = (args.report / 'webdriver.log').open('w')
process = None
def call(method, path, body=None):
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=35)
    try:
        connection.request(method, path, None if body is None else json.dumps(body), {'Content-Type': 'application/json'})
        response = connection.getresponse(); result = json.loads(response.read(32 * 1024 * 1024))
        if response.status >= 400: raise RuntimeError(result)
        return result.get('value')
    finally: connection.close()
def until(read, timeout=30):
    end = time.monotonic() + timeout
    while True:
        value = read()
        if value: return value
        if time.monotonic() >= end: raise TimeoutError('WebDriver condition deadline')
        time.sleep(0.025)
session = None
phase = 'paired-daemon-startup' if args.daemon else 'webdriver-startup'
try:
    if args.daemon:
        daemon = subprocess.Popen([str(companion), 'host', 'serve', '--profile', 'fixture'], env=env, stdout=daemon_log, stderr=subprocess.STDOUT)
    phase = 'webdriver-startup'
    process = subprocess.Popen([str(args.driver.resolve()), '--host=127.0.0.1', f'--port={port}'], env=env, stdout=log, stderr=subprocess.STDOUT)
    if daemon:
        phase = 'paired-daemon-startup'
        def daemon_ready():
            assert daemon.poll() is None, f'paired daemon exited during startup: {daemon.returncode}'
            files = list(Path(env['XDG_STATE_HOME']).rglob('owner.json'))
            if not files: return None
            value = json.loads(files[0].read_text())
            return value if value.get('endpoint_id') and value.get('host_epoch') else None
        daemon_metadata = until(daemon_ready)
        if args.foreign_binary:
            foreign = args.report / 'foreign-build'; foreign.mkdir()
            shutil.copy2(args.foreign_binary, foreign / 'rsi-desktop')
            (foreign / 'rsi').symlink_to(companion)
            rejected = subprocess.run([str((foreign / 'rsi-desktop').resolve()), '--assets', str(args.assets.resolve()), '--host-profile', 'fixture'], env=env, capture_output=True, text=True, timeout=45)
            (args.report / 'foreign-build.log').write_text(rejected.stdout + rejected.stderr)
            assert rejected.returncode != 0, 'foreign family unexpectedly attached'
            assert daemon.poll() is None and daemon_ready() == daemon_metadata
    phase = 'webdriver-startup'
    def ready():
        try: return call('GET', '/status')
        except (OSError, http.client.HTTPException): return None
    until(ready)
    phase = 'webview-startup'
    result = call('POST', '/session', {'capabilities': {'alwaysMatch': {'pageLoadStrategy': 'none' if args.startup_close else 'normal', 'webkitgtk:browserOptions': {
        'binary': str(args.binary.resolve()), 'args': ['--assets', str(args.assets.resolve()), '--host-profile', 'fixture']}}}})
    session = result['sessionId']; root = f'/session/{session}'
    phase = 'product-scenario'
    def script(source, arguments=None): return call('POST', root + '/execute/sync', {'script': source, 'args': arguments or []})
    def element(css): return call('POST', root + '/element', {'using': 'css selector', 'value': css})
    def eid(value): return next(iter(value.values()))
    def click(css): call('POST', root + f'/element/{eid(element(css))}/click', {})
    def fill(css, value):
        item = until(lambda: script(r'const selector=arguments[0], label=selector.match(/aria-label="([^"]+)"/)?.[1],e=document.querySelector(selector)||[...document.querySelectorAll("label")].find(e=>e.firstChild?.textContent.trim()===label)?.control;return e&&!e.disabled&&e.getBoundingClientRect().width>0?e:null', [css]))
        identity = eid(item)
        before = script('return {value:arguments[0].value,events:window.fixtureInputEvents?.length??0}', [item])
        call('POST', root + f'/element/{identity}/value', {'text': '\ue009a\ue000\ue003'})
        until(lambda: script('return arguments[0].value', [item]) == '')
        if css == 'textarea[aria-label="Main message"]' and before['value']:
            until(lambda: script('return window.fixtureInputEvents.slice(arguments[0]).some(e=>e.trusted&&e.length===0)', [before['events']]))
        call('POST', root + f'/element/{identity}/value', {'text': value, 'value': list(value)})
        until(lambda: script('return arguments[0].value', [item]) == value)
    def button(text):
        item = until(lambda: script(r'return [...document.querySelectorAll("button")].find(b=>(b.getAttribute("aria-label")||b.textContent.trim())===arguments[0]&&!b.disabled&&b.getBoundingClientRect().width>0&&b.getBoundingClientRect().height>0)||null', [text]))
        call('POST', root + f'/element/{eid(item)}/click', {})
    def painted():
        script(r'window.fixturePaint=false;requestAnimationFrame(()=>requestAnimationFrame(()=>window.fixturePaint=true));return true')
        until(lambda: script(r'return window.fixturePaint'))
    def screenshot(name):
        painted(); (args.report / name).write_bytes(base64.b64decode(call('GET', root + '/screenshot'), validate=True))
    if args.startup_close:
        until(lambda: script('return window.fixtureStartupStalled===true'))
        started = time.monotonic()
        script(r'void window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"});void window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"});return true')
        until(lambda: 'desktop: Application cleanup completed with status 1' in (args.report / 'webdriver.log').read_text(), 45)
        elapsed = time.monotonic() - started
        output = (args.report / 'webdriver.log').read_text()
        assert output.count('document drain timed out') == 1, output
        assert 29 <= elapsed < 45, elapsed
        if daemon:
            assert daemon.poll() is None and daemon_ready() == daemon_metadata
        (args.report / 'startup-close.json').write_text(json.dumps({'ok': True, 'elapsedSeconds': elapsed, 'closeRequests': 2, 'deadlineFailures': 1, 'cleanupStatus': 1, 'borrowedDaemonPreserved': bool(daemon)}, indent=2))
        session = None
        raise SystemExit(0)
    until(lambda: script(r'return document.querySelector("#workbench")?.hidden===false'), 45)
    script(r'''window.fixtureSubmissions=[];window.fixtureInputEvents=[];const originalFetch=window.fetch;window.fetch=(path,options)=>{const route=String(path),tracked=/\/_call\/(?:prepare_submission|submit_submission|query_submission)$/.test(route),entry={route,bytes:typeof options?.body==='string'?options.body.length:null};if(tracked)window.fixtureSubmissions.push(entry);return originalFetch(path,options).then(response=>{if(tracked)entry.status=response.status;return response})};document.addEventListener('input',event=>{if(event.target.matches('textarea[aria-label="Main message"]'))window.fixtureInputEvents.push({length:event.target.value.length,trusted:event.isTrusted})});return true''')
    script(r'return document.querySelector(".nav-add summary")')
    item = script(r'return [...document.querySelectorAll("summary")].find(b=>b.textContent.trim()==="Add workspace")')
    call('POST', root + f'/element/{eid(item)}/click', {})
    fill('[aria-label="Server directory"]', str(workspace.resolve())); button('Add workspace')
    until(lambda: script(r'return document.querySelector("#workspaces .nav-item")'))
    button('Settings')
    if not secret:
        script(r'const e=document.querySelector("select[aria-label=Provider]");e.value="openai-compatible";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    button('Check credential')
    until(lambda: script(r'return document.body.textContent.includes("configured · read only")'))
    fill('[aria-label="Deployment name"]', 'desktop-provider')
    fill('[aria-label="Provider endpoint"]', 'https://api.deepseek.com' if secret else f'http://127.0.0.1:{provider.server_port}')
    if secret:
        script(r'const e=document.querySelector("select[aria-label=\"DeepSeek protocol\"]");e.value="chat-completions";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    else: fill('[aria-label="Request path"]', '/v1/chat/completions')
    fill('[aria-label="Model identifier 1"]', args.live_model or 'fixture-model')
    button('Apply provider')
    until(lambda: script(r'return document.body.textContent.includes("Desired 1 · Applied 1")'))
    script(r'const e=document.querySelector("select[aria-label=\"Default model\"]");e.selectedIndex=1;e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    until(lambda: script(r'return document.body.textContent.includes("default_model · confirmed")&&!document.querySelector("select[aria-label=\"Default model\"]").disabled'))
    screenshot('setup.png'); button('Close settings'); click('#workspaces .nav-item')
    until(lambda: script(r'return document.querySelector("textarea[aria-label=\"Main message\"]")'))
    if secret: button('Trajectory')
    prompt = 'This is an isolated desktop integration test. Use the available bash tool to write the UTF-8 line "rsi-live-ok" to milestone.txt in the current workspace, then use bash to read it back. Do not modify any other file. Reply LIVE_GUI_VERIFIED only after the tool has read the file successfully.' if secret else '桌面首次对话：请确认收到。'
    fill('textarea[aria-label="Main message"]', prompt)
    if args.refresh_during_click:
        script(r'''window.fixturePress={refreshed:false,clicks:0};const b=[...document.querySelectorAll('button')].find(e=>e.textContent==='Send ↗');b.addEventListener('mousedown',()=>{const before=b.firstChild;document.querySelector('textarea[aria-label="Main message"]').dispatchEvent(new Event('input',{bubbles:true}));window.fixturePress.refreshed=true;window.fixturePress.sameTextNode=b.firstChild===before},{once:true});b.addEventListener('click',()=>{window.fixturePress.clicks++},{capture:true});return true''')
    button('Send ↗')
    if args.refresh_during_click:
        pressed = script('return window.fixturePress')
        (args.report / 'refresh-during-click.json').write_text(json.dumps(pressed, indent=2))
        assert pressed == {'refreshed': True, 'sameTextNode': True, 'clicks': 1}, pressed
    expected = 'LIVE_GUI_VERIFIED' if secret else 'Desktop conversation verified'
    approvals = 0
    def completed():
        global approvals
        pending = script(r'return [...document.querySelectorAll(".pending button")].find(b=>b.textContent.startsWith("Review:"))||null')
        if pending:
            call('POST', root + f'/element/{eid(pending)}/click', {}); button('Allow once'); approvals += 1
        return script(r'return [...document.querySelectorAll(".message.assistant")].some(b=>b.textContent.includes(arguments[0]))&&document.querySelector(".pane-status")?.textContent==="Completed"', [expected])
    until(completed, 180 if secret else 45)
    until(lambda: script(r'return document.querySelectorAll("#sessions .session-row").length===1'))
    screenshot('conversation.png')
    if secret:
        assert not requests, requests
        assert (workspace / 'milestone.txt').read_bytes() == b'rsi-live-ok\n'
        transcript = script(r'return document.querySelector(".transcript").innerText')
        assert 'bash' in transcript
        (args.report / 'transcript.txt').write_text(transcript)
    else: assert requests and requests[-1]['model'] == 'fixture-model', requests
    if args.tasks:
        verify_tasks(script, button, fill, until, screenshot, workspace, args.report, task_provider)
    geometry = script(r'const input=document.querySelector("textarea[aria-label=\"Main message\"]"),send=[...document.querySelectorAll("button")].find(b=>b.textContent.trim()==="Send ↗"),r=send.getBoundingClientRect();return {input:input.getBoundingClientRect().width,overflow:document.documentElement.scrollWidth-innerWidth,sendHit:send.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),origin:location.origin}')
    assert geometry['input'] >= 180 and geometry['overflow'] <= 1 and geometry['sendHit'], geometry
    script(r'''window.nativeAdmission=null;(async()=>{const cases=[['/_frame',undefined],['/_ack',JSON.stringify({frame_id:'18446744073709551615'})],['/_ack','x'.repeat(1025)],['/_call/command',undefined],['/_frame?'+ 'x'.repeat(2049),undefined]];const results=[];for(const [path,body] of cases){const response=await fetch(path,{method:body===undefined?'GET':'POST',body});results.push({status:response.status,text:await response.text()})}window.nativeAdmission=results})().catch(e=>window.nativeAdmission={error:String(e)});return true''')
    admission = until(lambda: script('return window.nativeAdmission'))
    assert all(item['status'] == 409 for item in admission), admission
    (args.report / 'native-admission.json').write_text(json.dumps(admission, indent=2))
    if args.save_failure:
        script(r'''window.fixturePut=IDBObjectStore.prototype.put;IDBObjectStore.prototype.put=function(){throw new DOMException('Injected draft write failure','QuotaExceededError')};return true''')
        fill('textarea[aria-label="Main message"]', 'draft must survive failed close 中文')
        until(lambda: script(r'return document.querySelector(".draft-error")?.textContent.includes("Injected draft write failure")'))
        script(r'void window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"});return true')
        until(lambda: script(r'return document.querySelector("#notice")?.textContent.includes("Recover the draft before closing")'))
        deadline = time.monotonic() + 31
        while time.monotonic() < deadline:
            assert script(r'return document.querySelector("textarea[aria-label=\"Main message\"]")?.value') == 'draft must survive failed close 中文'
            time.sleep(.25)
        assert 'document drain timed out' not in (args.report / 'webdriver.log').read_text()
        screenshot('failed-save-preserved.png')
        script(r'IDBObjectStore.prototype.put=window.fixturePut;return true')
        button('Replace saved text and images')
        until(lambda: script(r'return !document.querySelector(".draft-error")'))
        assert script(r'return document.querySelector("textarea[aria-label=\"Main message\"]").value') == 'draft must survive failed close 中文'
        (args.report / 'failed-save-recovery.json').write_text(json.dumps({'windowPreserved': True, 'closeDeadlineCancelledBeyond30Seconds': True, 'inputRecovered': True, 'automaticReplay': False}, indent=2))
    if args.ack_timeout:
        script(r'''const original=window.fetch;window.fetch=(path,options)=>{if(String(path)==='/_ack'){window.heldNativeAck=JSON.parse(options.body);return new Promise(()=>{})}return original(path,options)};void fetch('/_call/command',{method:'POST',body:JSON.stringify({action:'refresh'})});return true''')
        held = until(lambda: script('return window.heldNativeAck'))
        until(lambda: 'desktop: Application cleanup completed with status 1' in (args.report / 'webdriver.log').read_text(), 45)
        assert 'document acknowledgement timed out' in (args.report / 'webdriver.log').read_text()
        (args.report / 'ack-timeout.json').write_text(json.dumps({'pendingFrame': held['frame_id'], 'cleanupFailureReported': True, 'staleAckRejected': True}, indent=2))
        # The window has already exited through the failed-lifetime path.
        session = None
        raise SystemExit(0)
    if args.restart: fill('textarea[aria-label="Main message"]', 'persistent unsent desktop draft 中文')
    if args.window_close:
        script(r'void window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"});return true')
    else: button('Close application')
    until(lambda: 'desktop: main-thread exit after Application cleanup' in (args.report / 'webdriver.log').read_text(), 45)
    if args.restart:
        call('DELETE', root)
        result = call('POST', '/session', {'capabilities': {'alwaysMatch': {'webkitgtk:browserOptions': {
            'binary': str(args.binary.resolve()), 'args': ['--assets', str(args.assets.resolve()), '--host-profile', 'fixture']}}}})
        session = result['sessionId']; root = f'/session/{session}'
        until(lambda: script(r'return document.querySelector("#workbench")?.hidden===false'), 45)
        until(lambda: script(r'return document.querySelectorAll("#sessions .session-row").length===1'))
        click('#sessions .session-row button')
        until(lambda: script(r'return document.querySelector("textarea[aria-label=\"Main message\"]")?.value==="persistent unsent desktop draft 中文"'))
        assert script(r'return document.querySelector(".transcript").textContent.includes(arguments[0])', [expected])
        screenshot('restarted.png'); button('Close application')
        until(lambda: (args.report / 'webdriver.log').read_text().count('desktop: main-thread exit after Application cleanup') == 2, 45)
    if daemon:
        assert daemon.poll() is None, 'desktop shutdown stopped its borrowed daemon'
        assert daemon_ready() == daemon_metadata, 'desktop reconnect replaced the daemon generation'
    report = {'ok': True, 'capabilities': result['capabilities'], 'requests': requests, 'geometry': geometry, 'actualTyping': True, 'cleanupBeforeMainThreadExit': True, 'windowClose': args.window_close, 'restart': args.restart, 'liveModel': args.live_model, 'approvals': approvals, 'verifiedFileBytes': 12 if secret else None, 'borrowedDaemonPreserved': bool(daemon), 'foreignFamilyRejectedWithSameCompanion': bool(args.foreign_binary), 'daemonIdentity': daemon_metadata}
    (args.report / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)); print(json.dumps(report, ensure_ascii=False))
except Exception as error:
    record_failure(args.report, phase, error, daemon, process, secret)
    if session:
        try:
            (args.report / 'failure.html').write_text(script(r'return document.documentElement.outerHTML'))
            (args.report / 'failure-state.json').write_text(json.dumps({'requests': requests, 'document': script(r'return {submissions:window.fixtureSubmissions,events:window.fixtureInputEvents,input:[...document.querySelectorAll("textarea")].map(e=>({label:e.getAttribute("aria-label"),value:e.value,disabled:e.disabled})),status:document.querySelector(".pane-status")?.textContent,notice:document.querySelector("#notice")?.textContent}')}, ensure_ascii=False, indent=2))
            screenshot('failure.png')
        except Exception: pass
    raise
finally:
    failure = sys.exception()
    def close_session():
        if session:
            try: call('DELETE', f'/session/{session}')
            except (OSError, RuntimeError, http.client.HTTPException): pass
    def stop_webdriver():
        if process:
            process.terminate()
            try: process.wait(timeout=10)
            except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=10)
    def stop_daemon():
        if daemon and daemon.poll() is None:
            subprocess.run([str(companion), 'host', 'stop'], env=env, check=True, timeout=45)
    def wait_daemon():
        if daemon:
            try: daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=10)
                raise
    run_cleanup([
        ('task provider', task_provider.close), ('WebDriver session', close_session),
        ('WebDriver process', stop_webdriver), ('WebDriver log', log.close),
        ('provider shutdown', provider.shutdown), ('provider socket', provider.server_close),
        ('daemon stop', stop_daemon), ('daemon wait', wait_daemon),
        ('daemon log', lambda: daemon_log.close() if daemon_log else None),
        ('runtime directory', runtime_directory.cleanup),
        ('evidence redaction', lambda: redact_evidence(args.report, secret, failure)),
    ], failure)
