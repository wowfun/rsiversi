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
from dock import verify as verify_dock
from presentation import verify as verify_presentation
from alignment import verify as verify_alignment
from queue_scenario import verify as verify_queue
from tasks import ProviderControl, verify as verify_tasks
from workflow import configure as configure_workflow, provider_reply as workflow_reply, verify as verify_workflow
from plan_review import provider_reply as plan_reply, verify as verify_plan_review
from pressure import verify_writes
from controls import COMPOSER_FOCUS, fill as fill_input, asset_pressure, file_has_bytes, configure_live_limits
from external import configure as configure_external, verify as verify_external, provider_reply as external_reply, delegation as verify_delegation
from profiles import verify as verify_profiles
from history import verify as verify_history
from language import verify as verify_language
from workspace_review import verify as verify_review, reply as review_reply
from attention import verify as verify_attention, provider_reply as attention_reply
from file_previews import verify as verify_previews
from native_window import request_close as native_window_close
from session_export import verify as verify_export
from automation import configure as configure_automation, verify as verify_automation
from typed_results import verify as verify_typed, provider_reply as typed_reply

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from live import read_deepseek_key, live_limits

parser = argparse.ArgumentParser(description=__doc__)
for option in ('binary', 'driver', 'assets', 'report'):
    parser.add_argument('--' + option, type=Path, required=True)
parser.add_argument('--live-env-file', type=Path)
parser.add_argument('--live-model')
parser.add_argument('--window-close', action='store_true')
parser.add_argument('--restart', action='store_true')
parser.add_argument('--host-loss', action='store_true')
parser.add_argument('--reload', action='store_true')
parser.add_argument('--automation-fixture', type=Path)
parser.add_argument('--foreign-bundle', type=Path)
parser.add_argument('--ack-timeout', action='store_true')
parser.add_argument('--save-failure', action='store_true')
parser.add_argument('--close-timeout', action='store_true')
parser.add_argument('--refresh-during-click', action='store_true')
parser.add_argument('--tasks', action='store_true')
parser.add_argument('--plan-review', action='store_true')
parser.add_argument('--workflows', action='store_true')
parser.add_argument('--terminals', action='store_true')
parser.add_argument('--dock', action='store_true')
parser.add_argument('--presentation', action='store_true')
parser.add_argument('--ui-alignment', action='store_true')
parser.add_argument('--queue', action='store_true')
parser.add_argument('--system-theme', choices=('light','dark'))
parser.add_argument('--external', action='store_true')
parser.add_argument('--profiles', action='store_true')
parser.add_argument('--history', action='store_true')
parser.add_argument('--workspace-review', action='store_true')
parser.add_argument('--language', type=Path)
parser.add_argument('--attention', action='store_true')
parser.add_argument('--typed-results', action='store_true')
parser.add_argument('--file-previews', action='store_true')
parser.add_argument('--export', action='store_true')
args = parser.parse_args()
if bool(args.live_env_file) != bool(args.live_model):
    parser.error('live mode requires both an authorized environment file and a model')
if args.queue and args.live_env_file: parser.error('queue scenario requires its deterministic held response')
if args.tasks and (args.live_env_file or args.close_timeout or args.ack_timeout):
    parser.error('task mechanisms require the ordinary deterministic product scenario')
if args.dock and not args.terminals: parser.error('dock gestures require the real terminal scenario')
if args.host_loss and not args.restart: parser.error('--host-loss requires --restart')
if args.ack_timeout and args.restart: parser.error('ACK timeout and clean restart are distinct scenarios')
if args.close_timeout and (args.restart or args.save_failure or args.ack_timeout or args.live_env_file or args.refresh_during_click):
    parser.error('close timeout is a separate deterministic scenario')
secret = None
limits = live_limits() if args.live_env_file else None
if args.live_env_file:
    secret = read_deepseek_key(args.live_env_file)
args.report.mkdir(parents=True, exist_ok=False)
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
        if args.queue:
            prompt = next((message.get('content','') for message in reversed(body.get('messages', [])) if message.get('role') == 'user'), '')
            if isinstance(prompt, list): prompt = ''.join(part.get('text','') for part in prompt)
            requests[-1]['prompt'] = prompt
            if prompt == 'Desktop queue hold active':
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
                try:
                    self.wfile.write(('data: '+json.dumps({'choices':[{'delta':{'role':'assistant','content':'Queue fixture is waiting.'},'finish_reason':None}]})+'\n\n').encode()); self.wfile.flush()
                    for _ in range(1200):
                        time.sleep(0.1); self.wfile.write(b': keepalive\n\n'); self.wfile.flush()
                except (BrokenPipeError,ConnectionResetError): pass
                return
        if args.tasks and task_provider.handle(self, body):
            return
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
        delta = {'choices': [{'delta': {'role': 'assistant', 'content': 'Desktop conversation verified. 中文输入已收到。'}, 'finish_reason': None}]}
        if args.ui_alignment: delta['choices'][0]['delta']['content'] += '\n\n```text\nnative clipboard verified\n```'
        done = {'choices': [{'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 12}}
        delegation = (workflow_reply(body) if args.workflows else None) or (plan_reply(body) if args.plan_review else None) or (review_reply(body) if args.workspace_review else None) or (typed_reply(body) if args.typed_results else None) or (attention_reply(body) if args.attention else None) or (external_reply(body) if args.external else None)
        if delegation:
            delta['choices'][0]['delta'] = {'role':'assistant', **delegation}
            done['choices'][0]['finish_reason'] = 'tool_calls'
        self.wfile.write(('data: ' + json.dumps(delta) + '\n\ndata: ' + json.dumps(done) + '\n\ndata: [DONE]\n\n').encode())
provider = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
env = {key: value for key, value in os.environ.items() if key in ('PATH', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'LANG', 'XAUTHORITY')}
env.update(TAURI_WEBVIEW_AUTOMATION='true', RSI_OPENAI_COMPATIBLE_API_KEY='isolated-desktop-fixture')
if secret: env['DEEPSEEK_API_KEY'] = secret
if args.system_theme: env['GTK_THEME'] = 'Adwaita:dark' if args.system_theme == 'dark' else 'Adwaita'
for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'XDG_RUNTIME_DIR'):
    path = args.report / key.lower(); path.mkdir(mode=0o700); env[key] = str(path.resolve())
runtime_directory = tempfile.TemporaryDirectory(prefix='rsi-desktop-', dir='/tmp')
env['XDG_RUNTIME_DIR'] = runtime_directory.name
config = Path(env['XDG_CONFIG_HOME']) / 'rsi'
host = config / 'host-profiles/fixture'; host.mkdir(parents=True)
(host / 'host.profile.toml').write_text('format = 1\nsteps = []\n')
(config / 'rsi.toml').write_text('format=1\n[service]\nkind="catalog"\nid="fixture"\n')
homes = ['--config-home', str(config.resolve()), '--state-home', str((Path(env['XDG_STATE_HOME'])/'rsi').resolve()), '--cache-home', str((Path(env['XDG_CACHE_HOME'])/'rsi').resolve())]
desktop_arguments = homes + ['--assets', str(args.assets.resolve())]
if args.automation_fixture: configure_automation(host / 'host.profile.toml', args.automation_fixture)
if args.profiles:
    editable = config / 'host-profiles/editable'; editable.mkdir()
    editable_source = editable / 'host.profile.toml'
    editable_source.write_text('format = 1\nsteps = []\n')
(config / 'settings.json').write_text(json.dumps({'rsi.agent':{'require_approval':True} if args.attention else {}}))
if args.workflows: configure_workflow(config,host/'host.profile.toml')
workspace = args.report / 'workspace'; workspace.mkdir()
if args.language:
    language_position = json.loads(subprocess.check_output(['python3', str(Path(__file__).parent.parent/'lsp/prepare.py'), str(workspace), str(host/'host.profile.toml'), str(args.language)],text=True))
if args.workspace_review:
    def git(*arguments): subprocess.run(['/usr/bin/git', *arguments], cwd=workspace, check=True, capture_output=True)
    git('init', '--quiet'); (workspace / 'card.txt').write_text('committed\n'); git('add', '.')
    git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')
    (workspace / 'card.txt').write_text('before\n')
if args.external: configure_external(host / 'host.profile.toml', workspace.resolve(), Path(env['XDG_STATE_HOME']) / 'rsi')
daemon = None
daemon_log = None
daemon_metadata = None
companion = args.binary.resolve().with_name('rsi')
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
phase = 'resident-host-startup'
try:
    receipt_path = companion.with_name('receipt.json')
    receipt = json.loads(receipt_path.read_text()) if receipt_path.is_file() else None
    if receipt:
        assert hashlib.sha256(companion.with_name('build-family.json').read_bytes()).hexdigest() == receipt['family_sha256']
        for name in ('rsi', 'rsi-desktop'):
            assert hashlib.sha256(companion.with_name(name).read_bytes()).hexdigest() == receipt['artifacts'][name]
    with companion.open('rb') as executable: native_elf = executable.read(4) == b'\x7fELF'
    if native_elf:
        libraries = subprocess.check_output(['ldd', str(companion)], text=True)
        assert not re.search(r'lib(?:gtk|webkit|javascriptcore)', libraries, re.I), libraries
        (args.report / 'headless-libraries.txt').write_text(libraries)
    def artifact(path):
        with path.open('rb') as source: digest = hashlib.file_digest(source, 'sha256').hexdigest()
        return {'path': str(path.resolve()), 'bytes': path.stat().st_size, 'sha256': digest}
    native_binary = args.foreign_bundle.resolve()/'rsi-desktop' if args.foreign_bundle else args.binary
    web_assets = args.foreign_bundle.resolve()/'assets' if args.foreign_bundle else args.assets
    (args.report/'native-artifacts.json').write_text(json.dumps({
        'core': artifact(companion), 'desktop': artifact(native_binary),
        'web': artifact(web_assets/'rsi-build.json'),
    }, indent=2))
    daemon = subprocess.Popen([str(companion), *homes, '--watch', 'host', 'serve'], env=env, stdout=daemon_log, stderr=subprocess.STDOUT)
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
        if args.foreign_bundle:
            foreign = args.foreign_bundle.resolve()
            foreign_receipt = json.loads((foreign / 'receipt.json').read_text())
            assert receipt and foreign_receipt['family_sha256'] != receipt['family_sha256']
            for name in ('rsi', 'rsi-desktop'):
                assert hashlib.sha256((foreign / name).read_bytes()).hexdigest() == foreign_receipt['artifacts'][name]
            args.binary = foreign / 'rsi-desktop'
            desktop_arguments = homes + ['--assets', str((foreign / 'assets').resolve())]
    phase = 'webdriver-startup'
    def ready():
        try: return call('GET', '/status')
        except (OSError, http.client.HTTPException): return None
    until(ready)
    phase = 'webview-startup'
    result = call('POST', '/session', {'capabilities': {'alwaysMatch': {'pageLoadStrategy': 'normal', 'webkitgtk:browserOptions': {
        'binary': str(args.binary.resolve()), 'args': desktop_arguments}}}})
    session = result['sessionId']; root = f'/session/{session}'
    phase = 'product-scenario'
    def script(source, arguments=None): return call('POST', root + '/execute/sync', {'script': source, 'args': arguments or []})
    def element(css): return call('POST', root + '/element', {'using': 'css selector', 'value': css})
    def eid(value): return next(iter(value.values()))
    def click(css): call('POST', root + f'/element/{eid(element(css))}/click', {})
    def fill(css, value):
        def keys(item, text):
            call('POST', root + f'/element/{eid(item)}/value', {'text': text, 'value': list(text)})
        fill_input(script, keys, until, css, value)
    def button(text):
        if text == 'Close details':
            item=script('return document.querySelector("[data-dockkit-float-active=true] [data-dockkit-float-close], [data-dockkit-pane-active=true] [aria-selected=true] [data-dockkit-tab-close]")')
            assert item, 'No active resource close control'
            tab=script('return arguments[0].dataset.dockkitTabClose || arguments[0].closest("[data-dockkit-content]").dataset.dockkitContent',[item])
            call('POST',root+f'/element/{eid(item)}/click',{})
            until(lambda: script('return !document.querySelector(`[data-dockkit-content="${arguments[0]}"]`)',[tab]))
            return
        if text in ('Session commands', 'Earlier history', 'Back to live', 'Service extensions'):
            menu = script('return document.querySelector(".pane.selected .session-menu:not([open]) > summary")')
            if menu:
                call('POST',root+f'/element/{eid(menu)}/click',{})
        if text in ('Workspace files', 'Workspace changes', 'Goal'):
            if script(r'return document.querySelector("[aria-label=\"Toggle resources\"]").getAttribute("aria-expanded")!=="true"'):
                button('Toggle resources')
            if not script('return [...document.querySelectorAll("button")].some(b=>b.textContent.trim()===arguments[0]&&b.getBoundingClientRect().height>0)',[text]):
                count=script('return document.querySelectorAll("[data-dockkit-tab]").length')
                button('Open resources')
                until(lambda: script('return document.querySelectorAll("[data-dockkit-tab]").length')==count+1)
        if text in ('Compact','Standard','Detailed','Verbose'):
            script(r'''document.querySelector('.conversation-menu').open=true;const e=document.querySelector('[aria-label="Detail level"]');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));document.querySelector('.conversation-menu').open=false;return true''',[text.lower()])
            return
        if text in ('@ File path','Reference session','Add images','Search history','Export'):
            script('document.querySelector(".pane.selected .composer-extras").open=true;return true')
        if text == 'Terminal': script('document.querySelector(".conversation-menu").open=true;return true')
        def attempt():
            item = script(r'return [...document.querySelectorAll(arguments[0]==="External agents"?".external-navigation>summary":"button")].find(b=>(arguments[0]==="Send"?b.dataset.testid==="composer-send":(b.getAttribute("aria-label")||b.textContent.trim())===arguments[0])&&!b.disabled&&b.getBoundingClientRect().width>0&&b.getBoundingClientRect().height>0)||null', [text])
            if item is None: return False
            try:
                call('POST', root + f'/element/{eid(item)}/click', {})
            except RuntimeError as error:
                reply = error.args[0] if error.args else None
                if isinstance(reply, dict) and reply.get('value', {}).get('error') == 'stale element reference':
                    return False
                raise
            return True
        until(attempt)
        if text == 'Terminal':
            until(lambda: script('return [...document.querySelectorAll("[data-dockkit-tab][aria-selected=true]")].some(e=>e.textContent.includes("Resources"))'))
            script('document.querySelector(".conversation-menu").open=false;return true')
    def painted():
        script(r'window.fixturePaint=false;requestAnimationFrame(()=>requestAnimationFrame(()=>window.fixturePaint=true));return true')
        until(lambda: script(r'return window.fixturePaint'))
    def screenshot(name):
        painted(); (args.report / name).write_bytes(base64.b64decode(call('GET', root + '/screenshot'), validate=True))
    if args.close_timeout:
        until(lambda: script('return document.querySelector("#workbench")?.hidden===false'))
        script("window.addEventListener('rsi-native-close', event => event.stopImmediatePropagation(), {capture:true}); return true")
        started = time.monotonic()
        native_window_close()
        native_window_close()
        until(lambda: 'desktop: cleanup completed with status 1' in (args.report / 'webdriver.log').read_text(), 45)
        elapsed = time.monotonic() - started
        output = (args.report / 'webdriver.log').read_text()
        assert output.count('document drain timed out') == 1, output
        assert 29 <= elapsed < 45, elapsed
        if daemon:
            assert daemon.poll() is None and daemon_ready() == daemon_metadata
        (args.report / 'close-timeout.json').write_text(json.dumps({'ok': True, 'elapsedSeconds': elapsed, 'closeRequests': 2, 'deadlineFailures': 1, 'cleanupStatus': 1, 'borrowedDaemonPreserved': bool(daemon)}, indent=2))
        session = None
        raise SystemExit(0)
    until(lambda: script(r'return document.querySelector("#workbench")?.hidden===false'), 45)
    def observe_document():
        script((Path(__file__).parent / 'transport-probe.js').read_text() + '\nreturn true')
        script(r'''window.fixtureSubmissions=[];window.fixtureInputEvents=[];fixtureTransport.observers.push((kind,data,request)=>{if(kind==='call'&&['prepare_submission','dispatch_submission','query_submission'].includes(data.method))window.fixtureSubmissions.push({method:data.method,bytes:typeof data.payload==='string'?data.payload.length:null});});document.addEventListener('input',event=>{if(event.target.matches('textarea[aria-label="Main message"]'))window.fixtureInputEvents.push({length:event.target.value.length,trusted:event.isTrusted})});return true''')
    observe_document()
    if args.profiles:
        verify_profiles(script, button, lambda item: call('POST', root + f'/element/{eid(item)}/click', {}), until, screenshot, editable_source, args.report)
    if args.external: verify_external(script, button, fill, until, screenshot, workspace, args.report)
    button('Choose workspace')
    until(lambda: script('return document.querySelector(".directory-columns[aria-busy=false]")'))
    edit = script(r'return document.querySelector("[aria-label=\"Edit directory path\"]")')
    if edit: call('POST', root + f'/element/{eid(edit)}/click', {})
    fill('[aria-label="Directory path"]', str(workspace.resolve()))
    click('.directory-picker footer button:last-child')
    until(lambda: script('return !document.querySelector(".directory-picker")'))
    button('Settings'); button('Models'); button('Add model provider')
    def summary(label):
        item = until(lambda: script('return [...document.querySelectorAll("summary")].find(e=>e.textContent.trim()===arguments[0])', [label]))
        call('POST', root + f'/element/{eid(item)}/click', {})
    if not secret:
        button('Custom model API')
        fill('[aria-label="Provider ID"]', 'desktop-provider')
        fill('[aria-label="Base URL"]', f'http://127.0.0.1:{provider.server_port}')
    summary('Customized settings')
    if secret:
        fill('[aria-label="Base URL"]', 'https://api.deepseek.com')
        script(r'const e=document.querySelector("select[aria-label=\"API protocol\"]");e.value="chat-completions";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    else: fill('[aria-label="Request path"]', '/v1/chat/completions')
    summary('Connection identifiers')
    fill('details details [aria-label="Provider ID"]', 'desktop-provider')
    fill('[aria-label="Credential slot"]', 'default')
    if not secret: button('Add model')
    fill('[aria-label="Model ID 1"]', args.live_model or 'fixture-model')
    if secret: configure_live_limits(script, fill, limits)
    button('Create provider')
    until(lambda: script(r'return !document.querySelector("form[aria-label=\"Add model provider\"]")'))
    model_label = f'{args.live_model or "fixture-model"} · desktop-provider'
    until(lambda: script(r'const e=document.querySelector("select[aria-label=\"Default model\"]");return e&&!e.disabled&&e.selectedOptions[0]?.textContent===arguments[0]', [model_label]))
    screenshot('setup.png')
    button('Plugins')
    until(lambda: script(r'return document.querySelector(".plugin-row")'))
    revisions = script(r'return document.querySelector(".plugin-revisions").textContent')
    assert 'Desired ' in revisions and 'Observed ' in revisions, revisions
    button('Refresh plugin status')
    until(lambda: script(r'return document.querySelector(".plugin-row")'))
    button('Read MCP status')
    until(lambda: script(r'return document.querySelector("[aria-label=\"MCP connections\"]").textContent.includes("MCP catalog verified.")'))
    summary = script(r'return document.querySelector("[aria-label=\"Web retrieval\"] summary")')
    call('POST', root + f'/element/{eid(summary)}/click', {})
    button('Read Exa credential status')
    until(lambda: script(r'return !document.querySelector("[aria-label=\"Web retrieval\"] input[type=password]").disabled'))
    script(r'document.querySelector("[aria-label=\"Web retrieval\"]").scrollIntoView({block:"center"});return true')
    screenshot('plugins-retrieval.png')
    button('Web retrieval settings')
    flags = until(lambda: script(r'return [...document.querySelectorAll("input[type=checkbox]")].filter(e=>e.getAttribute("aria-label")?.startsWith("Settings / web_")).map(e=>({label:e.getAttribute("aria-label"),checked:e.checked}))'))
    assert len(flags) == 2 and all(not flag['checked'] for flag in flags), flags
    screenshot('retrieval-settings.png')
    (args.report / 'plugins.json').write_text(json.dumps({'status':'passed','revisions':revisions,'retrievalDefaultFlags':flags,'exaStatusRead':True}, indent=2))
    button('Close settings'); click('[aria-label="New conversation in workspace"]')
    until(lambda: script(r'return document.querySelector("textarea[aria-label=\"Main message\"]")'))
    if secret:
        click('.conversation-menu > summary')
        script(r'const e=document.querySelector("[aria-label=\"Detail level\"]");e.value="verbose";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
        click('.conversation-menu > summary')
    prompt = 'This is an isolated desktop integration test. Use the available bash tool to write exactly 12 UTF-8 bytes: "rsi-live-ok" followed by one LF newline to milestone.txt in the current workspace, then use bash to read it back. Do not modify any other file. Reply LIVE_GUI_VERIFIED only after the tool has read the file successfully.' if secret else '桌面首次对话：请确认收到。'
    input_events_before = script('return window.fixtureInputEvents.length')
    fill('textarea[aria-label="Main message"]', prompt)
    focus = script(COMPOSER_FOCUS, [input_events_before])
    assert focus['focused'] and focus['focusVisible'] and focus['trustedInput'], focus
    if focus['forcedColors']:
        assert focus['outlineWidth'] >= 2 and focus['outlineStyle'] != 'none', focus
    else:
        assert focus['outlineWidth'] == 0 and focus['outlineStyle'] == 'none', focus
    (args.report / 'composer-focus.json').write_text(json.dumps(focus, indent=2))
    screenshot('composer-focus.png')
    if args.refresh_during_click:
        script(r'''window.fixturePress={refreshed:false,clicks:0};const b=[...document.querySelectorAll('button')].find(e=>e.dataset.testid==='composer-send');b.addEventListener('mousedown',()=>{const before=b.firstChild;document.querySelector('textarea[aria-label="Main message"]').dispatchEvent(new Event('input',{bubbles:true}));window.fixturePress.refreshed=true;window.fixturePress.sameTextNode=b.firstChild===before},{once:true});b.addEventListener('click',()=>{window.fixturePress.clicks++},{capture:true});return true''')
    button('Send')
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
    if args.ui_alignment: verify_alignment(script,button,fill,until,screenshot,call,root,args.report,workspace)
    if args.queue: verify_queue(script, button, fill, until, screenshot, args.report, requests)
    if args.presentation: verify_presentation(script, button, fill, until, screenshot, args.report, args.system_theme)
    if args.automation_fixture: verify_automation(script, button, click, until, screenshot, args.report)
    if args.workflows: verify_workflow(script,button,lambda item: call('POST',root+f'/element/{eid(item)}/click',{}),fill,until,screenshot,args.report)
    if args.plan_review: verify_plan_review(script, button, fill, until, screenshot, args.report)
    if args.export: verify_export(script, button, fill, until, screenshot, args.report, requests)
    if args.file_previews:
        verify_previews(script, button, fill, until, screenshot, lambda item: call('POST', root + '/frame', {'id': item}), workspace, args.report)
    if args.language: verify_language(script, button, fill, until, screenshot, args.report, language_position, workspace)
    if args.workspace_review: verify_review(script, button, fill, until, screenshot, workspace, args.report)
    if args.history: verify_history(script, button, fill, until, screenshot, args.report)
    if args.attention: verify_attention(script, button, fill, until, screenshot, args.report)
    if args.typed_results: verify_typed(script, button, lambda item: call('POST', root + f'/element/{eid(item)}/click', {}), fill, until, screenshot, args.report)
    if args.external: verify_delegation(script, button, fill, until, screenshot, workspace, args.report)
    if args.terminals:
        styles_before = script('return document.adoptedStyleSheets.length')
        script(r'''window.fixtureTerminalCommands=[];fixtureTransport.observers.push((kind,data)=>{if(kind==='call'&&data.method==='terminal'){const input=JSON.parse(data.payload);window.fixtureTerminalCommands.push(input.request);if(input.request?.type==='read'){window.fixtureTerminalRead=data.payload;window.fixtureMainRead??=data.payload}}if(kind==='frame'){window.fixtureFrameSeen=true;window.fixtureAssetOffer=data.assets}});return true''')
        button('Terminal'); button('New terminal')
        until(lambda: script('return document.querySelector(".terminal-authority")?.textContent==="You have control"'))
        typography = script('const s=getComputedStyle(document.querySelector(".xterm-rows"));return {family:s.fontFamily,size:s.fontSize,space:s.whiteSpace}')
        assert 'monospace' in typography['family'] and typography['size'] == '12px' and typography['space'] == 'pre', typography
        def terminal_keys(text):
            item = element('.terminal-screen .xterm-helper-textarea')
            call('POST', root + f'/element/{eid(item)}/value', {'text': text + '\ue007', 'value': list(text + '\ue007')})
        until(lambda: script('return Boolean(window.fixtureTerminalRead)'))
        script('void fixtureTransport.call("command",JSON.stringify({action:"refresh"}));return true')
        until(lambda: script('return window.fixtureFrameSeen===true'))
        script('void fixtureTransport.call("command",JSON.stringify({action:"refresh"}));return true')
        until(lambda: script('return Boolean(window.fixtureAssetOffer?.catalog)'))
        script(r'''window.fixturePressure={responses:[],busy:0,apiCapacity:0,pending:0,stopped:false,ready:false};
            const pressure=window.fixturePressure,input=JSON.parse(window.fixtureTerminalRead),terminals=[],followers=[];
            const request=command=>fixtureTransport.request('terminal',JSON.stringify({...input,request:command}));
            const required=async command=>{const reply=await request(command);if(reply.error)throw Error(reply.error);return JSON.parse(reply.result)};
            const read=async (attachment,probe=false)=>{let ack=null;while(!pressure.stopped){pressure.pending++;try{const reply=await request({type:'read',attachment,ack});const capacity=reply.error?.includes('API capacity is exhausted');if(capacity)pressure.apiCapacity++;const busy=reply.notAdmitted===true&&reply.retryable===true;if(busy)pressure.busy++;if(pressure.responses.length<256)pressure.responses.push(reply);if(!reply.error)ack=JSON.parse(reply.result).ack;else if(!busy&&!capacity&&!(probe&&reply.error.includes('Terminal attachment does not belong')))throw Error(reply.error);}finally{pressure.pending--}await new Promise(resolve=>setTimeout(resolve,10))}};
            window.fixturePressureCleanup=async()=>{pressure.stopped=true;await window.fixturePressureReaders;for(const terminal of terminals)await required({type:'close',terminal});pressure.cleaned=true};
            (async()=>{for(let i=0;i<4;i++){const created=await required({type:'create',size:{rows:24,columns:80}});terminals.push(created.value.terminal.id);followers.push(created.value.id);for(let j=1;j<(i===3?7:8);j++)followers.push((await required({type:'attach',terminal:created.value.terminal.id})).value.id)}pressure.ready=true;window.fixturePressureReaders=Promise.all([...followers.map(id=>read(id)),...Array.from({length:8},()=>read("absent-pressure-probe",true))]).catch(error=>{pressure.error=String(error);pressure.stopped=true})})().catch(error=>pressure.error=String(error));return true''')
        pressure = until(lambda: script('if(window.fixturePressure.error)throw Error(window.fixturePressure.error);return window.fixturePressure.busy>0?window.fixturePressure:null'))
        assert pressure['pending'] > 0, pressure
        # Re-evaluate one current frozen renderer URL, bypassing the module cache.
        script(r'''window.fixtureAssetPressure=null;const offer=window.fixtureAssetOffer;if(!offer?.catalog?.renderers.length)throw Error('no renderer offer');const path='/rsi-renderers/'+offer.revision+'/'+offer.catalog.renderers[0].entry;(async()=>{const response=await fetch('/app.js');if(!response.ok)throw Error('asset rejected');await response.arrayBuffer();const renderer=await fetch(path,{cache:'no-store'});if(!renderer.ok)throw Error('renderer rejected: '+renderer.status);await renderer.arrayBuffer();await import(path+'#native-pressure');window.fixtureAssetPressure={status:response.status,rendererStatus:renderer.status,module:path,pending:window.fixturePressure.pending}})().catch(error=>window.fixtureAssetPressure={error:String(error)});return true''')
        assets = asset_pressure(script, until)
        assert 'error' not in assets and assets['status'] == 200 and assets['pending'] > 0, assets
        terminal_keys("printf 'native-pty-ok' >> native-pty-result.txt; printf 'Native PTY 界\\n'")
        until(lambda: file_has_bytes(workspace / 'native-pty-result.txt', b'native-pty-ok'))
        assert (workspace / 'native-pty-result.txt').read_text() == 'native-pty-ok'
        until(lambda: script('return document.querySelector(".xterm-rows")?.textContent.includes("Native PTY 界")'))
        script('void window.fixturePressureCleanup().catch(error=>window.fixturePressure.error=String(error));return true')
        until(lambda: script('return window.fixturePressure.cleaned===true'))
        pressure = script('return window.fixturePressure')
        assert 'error' not in pressure, pressure
        busy = [item for item in pressure['responses'] if item.get('notAdmitted') and item.get('retryable')]
        assert pressure['busy'] > 0 and busy, pressure
        assert (workspace / 'native-pty-result.txt').read_bytes() == b'native-pty-ok'
        (args.report / 'terminal-pressure.json').write_text(json.dumps({'readPressure':pressure,'asset':assets,'exactBytes':'native-pty-ok'}, indent=2))
        verify_writes(script, button, terminal_keys, until, workspace, args.report)
        screenshot('terminal-writer.png')
        if args.dock: verify_dock(script, button, until, screenshot, call, root, eid, args.report)
        button('Hide terminal panel')
        until(lambda: script('return document.adoptedStyleSheets.length') == styles_before)
        button('Terminal'); button('Terminal 1 · running')
        until(lambda: script('return document.querySelector(".terminal-authority")?.textContent==="Read only"'))
        until(lambda: script('return document.querySelector(".xterm-rows")?.textContent.includes("Native PTY 界")'))
        screenshot('terminal-readonly.png'); button('Take control')
        until(lambda: script('return document.querySelector(".terminal-authority")?.textContent==="You have control"'))
        terminal_keys('exit 7')
        until(lambda: script('return document.querySelector(".terminal-authority")?.textContent==="Exited 7"'))
        screenshot('terminal-exited.png'); button('Terminate terminal'); button('Hide terminal panel')
        (args.report / 'terminals.json').write_text(json.dumps({'status':'passed','native_bash':True,'readonly_reattach':True,'explicit_takeover':True,'exit_code':7,'closed':True},indent=2))
    if args.tasks:
        verify_tasks(script, button, fill, until, screenshot, workspace, args.report, task_provider)
    # Closing a Rust-owned detail is asynchronous even after native click delivery.
    until(lambda: script('return !document.querySelector("dialog[open]")'))
    geometry = script(r'const input=document.querySelector("textarea[aria-label=\"Main message\"]"),send=[...document.querySelectorAll("button")].find(b=>b.dataset.testid==="composer-send"),r=send.getBoundingClientRect();return {input:input.getBoundingClientRect().width,overflow:document.documentElement.scrollWidth-innerWidth,sendHit:send.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),origin:location.origin}')
    assert geometry['input'] >= 180 and geometry['overflow'] <= 1 and geometry['sendHit'], geometry
    admission = script("return Promise.all(['/_frame','/_ack','/_call/command'].map(async path=>{const reply=await fetch(path,{method:'POST'});return {path,status:reply.status}}))")
    # The static/API listener's credential boundary rejects the retired native
    # routes; native capabilities are available only through the Tauri bridge.
    assert all(item['status'] == 401 for item in admission), admission
    (args.report / 'retired-routes.json').write_text(json.dumps(admission, indent=2))
    if args.save_failure:
        script(r'''window.fixturePut=IDBObjectStore.prototype.put;IDBObjectStore.prototype.put=function(){throw new DOMException('Injected draft write failure','QuotaExceededError')};return true''')
        fill('textarea[aria-label="Main message"]', 'draft must survive failed close 中文')
        until(lambda: script(r'return document.querySelector(".draft-error")?.textContent.includes("Injected draft write failure")'))
        native_window_close()
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
        script("fixtureTransport.holdAck=true;void fixtureTransport.call('command',JSON.stringify({action:'refresh'}));return true")
        held = until(lambda: script('return fixtureTransport.held'))
        failure = until(lambda: script('return fixtureTransport.failures[0]'), 45)
        assert 'acknowledgement timed out' in failure, failure
        (args.report / 'ack-timeout.json').write_text(json.dumps({'pendingFrame':held['frame_id'],'workerFailure':failure}, indent=2))
    def select_retained_session():
        until(lambda: script(r'return Boolean(document.querySelector("#workspaces .workspace-toggle"))'))
        if script(r'return document.querySelector("#workspaces .workspace-toggle").getAttribute("aria-expanded")!=="true"'): click('#workspaces .workspace-toggle')
        until(lambda: script(r'return document.querySelectorAll("#sessions .session-row").length===1'))
        click('#sessions .session-row button')
    if args.reload:
        fill('textarea[aria-label="Main message"]', 'saved across document reload')
        until(lambda: script('return !document.querySelector(".draft-status")?.textContent.includes("Saving")'))
        call('POST',root+'/refresh',{})
        until(lambda: script('return document.querySelector("#workbench")?.hidden===false'),45)
        observe_document()
        select_retained_session()
        until(lambda: script(r'''return document.querySelector('textarea[aria-label="Main message"]')?.value==='saved across document reload';'''))
        screenshot('reloaded.png')
        (args.report/'reload.json').write_text(json.dumps({'reload':True,'draftRetained':True}))
    if args.restart:
        initial_origin = script('return location.origin')
        if args.host_loss:
            script(r'''return import('/drafts.js').then(({DraftStore})=>{
                const edit=DraftStore.prototype.edit,state=window.fixtureHostLossDraft={pending:0,closeRequested:false};
                const held=new Promise(resolve=>state.release=resolve);
                state.restore=()=>{DraftStore.prototype.edit=edit;};
                DraftStore.prototype.edit=async function(...args){state.pending++;await held;return edit.apply(this,args);};
                window.addEventListener('rsi-native-close',()=>{state.closeRequested=true;});return true;
            })''')
        fill('textarea[aria-label="Main message"]', 'persistent unsent desktop draft 中文')
    if args.host_loss:
        until(lambda: script('return window.fixtureHostLossDraft.pending>0'))
        # Kill only the fixture-owned foreground Host; exercise independent native
        # lease EOF and Worker failure, including their ordering race.
        daemon.kill()
        daemon.wait(timeout=15)
        until(lambda: script('return window.fixtureHostLossDraft.closeRequested'))
        assert 'desktop: cleanup completed' not in (args.report / 'webdriver.log').read_text(), 'native exit bypassed pending draft persistence'
        screenshot('host-loss-pending.png')
        (args.report/'host-loss-drain.json').write_text(json.dumps({'draftHeldBeforeLoss': True, 'nativeCloseRequested': True, 'windowRetainedUntilPersistence': True}))
        script('fixtureHostLossDraft.restore();fixtureHostLossDraft.release();return true')
    elif args.window_close or args.ack_timeout:
        denied = script('return window.__TAURI_INTERNALS__.invoke("plugin:window|close",{label:"main"}).then(()=>"unexpectedly allowed",error=>String(error))')
        assert 'not allowed' in denied.lower(), denied
        (args.report / 'no-window-ipc.json').write_text(json.dumps({'command_denied': denied, 'native_close': True}, indent=2))
        native_window_close()
    else: button('Close application')
    initial_status = 1 if args.host_loss else 0
    until(lambda: f'desktop: cleanup completed with status {initial_status}' in (args.report / 'webdriver.log').read_text(), 45)
    if args.restart:
        call('DELETE', root)
        if args.host_loss:
            previous_epoch = daemon_metadata['host_epoch']
            daemon = subprocess.Popen([str(companion), *homes, '--watch', 'host', 'serve'], env=env, stdout=daemon_log, stderr=subprocess.STDOUT)
            daemon_metadata = until(lambda: (value if value and value['host_epoch'] != previous_epoch else None) if (value := daemon_ready()) else None, 60)
            (args.report/'host-loss.json').write_text(json.dumps({'hostLoss': True, 'oldEpoch': previous_epoch, 'newEpoch': daemon_metadata['host_epoch'], 'nativeCleanupStatus': 1}))
        result = call('POST', '/session', {'capabilities': {'alwaysMatch': {'webkitgtk:browserOptions': {
            'binary': str(args.binary.resolve()), 'args': desktop_arguments}}}})
        session = result['sessionId']; root = f'/session/{session}'
        until(lambda: script(r'return document.querySelector("#workbench")?.hidden===false'), 45)
        observe_document()
        assert script('return location.origin') == initial_origin, 'Desktop restart changed the storage Origin'
        select_retained_session()
        until(lambda: script(r'return document.querySelector("textarea[aria-label=\"Main message\"]")?.value==="persistent unsent desktop draft 中文"'))
        assert script(r'return document.querySelector(".transcript").textContent.includes(arguments[0])', [expected])
        screenshot('restarted.png'); button('Close application')
        (args.report / 'restart.json').write_text(json.dumps({'origin': initial_origin, 'draftRetained': True, 'conversationRetained': True}))
        until(lambda: (args.report / 'webdriver.log').read_text().count('desktop: cleanup completed with status 0') == (1 if args.host_loss else 2), 45)
    if daemon:
        assert daemon.poll() is None, 'desktop shutdown stopped its borrowed daemon'
        assert daemon_ready() == daemon_metadata, 'desktop reconnect replaced the daemon generation'
    report = {'ok': True, 'capabilities': result['capabilities'], 'requests': requests, 'geometry': geometry, 'actualTyping': True, 'cleanupBeforeMainThreadExit': True, 'windowClose': args.window_close, 'restart': args.restart, 'hostLoss': args.host_loss, 'liveModel': args.live_model, 'approvals': approvals, 'verifiedFileBytes': 12 if secret else None, 'borrowedDaemonPreserved': bool(daemon), 'independentBuildAccepted': bool(args.foreign_bundle), 'daemonIdentity': daemon_metadata}
    (args.report / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)); print(json.dumps(report, ensure_ascii=False))
except Exception as error:
    record_failure(args.report, phase, error, daemon, process, secret)
    if session:
        try:
            (args.report / 'failure.html').write_text(script(r'return document.documentElement.outerHTML'))
            (args.report / 'failure-state.json').write_text(json.dumps({'requests': requests, 'document': script(r'return {writePressure:window.fixtureWritePressure,pressure:window.fixturePressure,assetPressure:window.fixtureAssetPressure,goal:window.fixtureGoal,detail:document.querySelector("#detail")?.textContent?.slice(0,8192),submissions:window.fixtureSubmissions,events:window.fixtureInputEvents,input:[...document.querySelectorAll("textarea")].map(e=>({label:e.getAttribute("aria-label"),value:e.value,disabled:e.disabled})),status:document.querySelector(".pane-status")?.textContent,notice:document.querySelector("#notice")?.textContent}')}, ensure_ascii=False, indent=2))
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
            subprocess.run([str(companion), *homes, 'host', 'stop'], env=env, check=True, timeout=45)
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
