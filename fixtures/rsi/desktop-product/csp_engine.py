"""Opt-in WebKitGTK custom-scheme CSP probe using captured native response bytes.

Run with /usr/bin/python3 under Xvfb; requires PyGObject and WebKit2 4.1 GI.
The product navigation gate excludes foreign parents, so this separate view tests
frame-ancestors directly, with an otherwise identical permissive negative control.
"""
import argparse
import json
from pathlib import Path
import re
import time
from urllib.parse import urlsplit
import gi

gi.require_version('Gtk', '3.0')
gi.require_version('WebKit2', '4.1')
gi.require_version('Soup', '3.0')
from gi.repository import Gio, GLib, Gtk, Soup, WebKit2

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--responses', type=Path, required=True)
parser.add_argument('--report', type=Path, required=True)
args = parser.parse_args()
responses = {item['path']: item for item in json.loads(args.responses.read_text())}
context = WebKit2.WebContext.new_ephemeral()
# Match Wry's native WebKit custom-protocol registration.
context.get_security_manager().register_uri_scheme_as_secure('rsi')
strip_ancestors = False
allow_nested = False
nested_requests = []
navigation_decisions = []

def serve(request):
    address = urlsplit(request.get_uri())
    if address.path == '/nested-target':
        nested_requests.append(request.get_uri())
        body, csp = '<!doctype html><p>Nested transport control</p>', None
    elif address.path in ('/preview-local.html', '/preview-online.html', '/not-preview.html'):
        selected = responses['/preview-online.html' if address.path == '/preview-online.html' else '/preview-local.html']
        body, csp = selected['body'], selected['csp']
        if allow_nested:
            csp = csp.replace("frame-src 'none'", 'frame-src rsi:').replace("object-src 'none'", 'object-src rsi:')
        if strip_ancestors:
            csp = re.sub(r'(?:^|;)\s*frame-ancestors\s+[^;]*', '', csp)
    else:
        body = '<!doctype html><html><body>Native CSP engine probe</body></html>'
        csp = responses['/']['csp'] if address.hostname == 'localhost' else None
    data = body.encode()
    response = WebKit2.URISchemeResponse.new(Gio.MemoryInputStream.new_from_bytes(GLib.Bytes.new(data)), len(data))
    response.set_status(200, 'OK')
    response.set_content_type('text/html; charset=utf-8')
    headers = Soup.MessageHeaders.new(Soup.MessageHeadersType.RESPONSE)
    headers.append('Cache-Control', 'no-store')
    if csp:
        headers.append('Content-Security-Policy', csp)
    response.set_http_headers(headers)
    request.finish_with_response(response)

context.register_uri_scheme('rsi', serve)
window = Gtk.Window()
view = WebKit2.WebView.new_with_context(context)
view.get_settings().set_enable_write_console_messages_to_stdout(True)
def observe_navigation(_view, decision, kind):
    if kind == WebKit2.PolicyDecisionType.NAVIGATION_ACTION:
        navigation_decisions.append(decision.get_navigation_action().get_request().get_uri())
    return False
view.connect('decide-policy', observe_navigation)
window.add(view)
window.show_all()

def wait(predicate, timeout=10):
    end = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > end:
            raise TimeoutError('WebKit probe condition')
        GLib.MainContext.default().iteration(False)
        time.sleep(.005)

def evaluate(code):
    result = []
    def completed(view, task, _):
        try:
            result.append((view.evaluate_javascript_finish(task).to_json(0), None))
        except Exception as error:
            result.append((None, error))
    view.evaluate_javascript(code, -1, None, None, None, completed, None)
    wait(lambda: bool(result))
    value, error = result[0]
    if error:
        raise error
    return json.loads(value)

loaded = []
view.connect('load-changed', lambda view, event: loaded.append(True) if event == WebKit2.LoadEvent.FINISHED else None)
evidence = []
try:
    for path in ('/preview-local.html', '/preview-online.html'):
        # The foreign/foreign control proves the channel and scripts work under
        # the same custom-scheme engine; the remaining foreign cases must not run.
        for origin, child_origin, permissive in [('localhost', 'localhost', False), ('foreign', 'foreign', True), ('foreign', 'localhost', True), ('foreign', 'localhost', False)]:
            strip_ancestors = permissive
            loaded.clear()
            nonce = str(len(evidence))
            view.load_uri('rsi://' + origin + '/?case=' + nonce)
            wait(lambda: bool(loaded))
            target = json.dumps('rsi://' + child_origin + path + '?case=' + nonce)
            evaluate("""window.probe={ready:false,rendered:false,loaded:false,violations:[],origin:location.origin,childOrigin:null};document.addEventListener('securitypolicyviolation',e=>probe.violations.push(e.effectiveDirective));const f=document.createElement('iframe');window.addEventListener('message',e=>{
                if(e.source!==f.contentWindow)return;
                if(e.data?.type==='probe-rendered'){probe.rendered=true;return;}
                if(e.data?.type!=='rsi-preview-ready')return;
                probe.ready=true;probe.childOrigin=e.origin;
                const channel=new MessageChannel();f.contentWindow.postMessage({type:'rsi-preview-connect'},'*',[channel.port2]);channel.port1.postMessage({type:'render',html:'<script>parent.postMessage({type:"probe-rendered"},"*")<\\/script>',assets:[]});channel.port1.close();
            });f.onload=()=>probe.loaded=true;f.src=""" + target + ";document.body.append(f);true")
            wait(lambda: evaluate('probe.loaded'))
            wait(lambda: evaluate('probe.ready'))
            expected = origin == child_origin
            if expected:
                wait(lambda: evaluate('probe.rendered'))
            else:
                # The bootstrap has signalled readiness; observe the rejected
                # channel for a full second, not merely iframe load completion.
                deadline = time.monotonic() + 1
                while time.monotonic() < deadline:
                    assert not evaluate('probe.rendered'), 'foreign parent rendered through rejected channel'
                    time.sleep(.01)
            result = evaluate('probe')
            assert result['rendered'] == expected, (path, origin, child_origin, permissive, result)
            evidence.append({'path': path, 'ancestor': origin, 'bootstrap_host':child_origin, 'frame_ancestors_removed': permissive, **result})
    nested = []
    for path in ('/preview-local.html', '/preview-online.html'):
        for permissive in (False, True):
            strip_ancestors = False
            allow_nested = permissive
            nested_requests.clear()
            navigation_decisions.clear()
            loaded.clear()
            nonce = f'nested-{len(nested)}'
            view.load_uri('rsi://localhost/?case=' + nonce)
            wait(lambda: bool(loaded))
            document = """<!doctype html><body><script>
            const violations=[];
            document.addEventListener('securitypolicyviolation',e=>{violations.push({directive:e.effectiveDirective,blocked:e.blockedURI});parent.postMessage({type:'nested-evidence',violations},'*')});
            for(const tag of ['iframe','object','embed']){
                const element=document.createElement(tag), url='rsi://foreign/nested-target?case=CASE&tag='+tag;
                if(tag==='object'){element.data=url;element.type='text/html'}else{element.src=url;if(tag==='embed')element.type='text/html'}
                document.body.append(element);
            }
            parent.postMessage({type:'nested-started'},'*');
            </script>""".replace('CASE', nonce)
            evaluate("""window.nested={started:false,violations:[]};const f=document.createElement('iframe');window.addEventListener('message',e=>{
                if(e.source!==f.contentWindow)return;
                if(e.data?.type==='nested-started')nested.started=true;
                if(e.data?.type==='nested-evidence')nested.violations=e.data.violations;
                if(e.data?.type==='rsi-preview-ready'){
                    const channel=new MessageChannel();f.contentWindow.postMessage({type:'rsi-preview-connect'},'*',[channel.port2]);channel.port1.postMessage({type:'render',html:""" + json.dumps(document) + """,assets:[]});channel.port1.close();
                }
            });f.src=""" + json.dumps('rsi://localhost' + path + '?case=' + nonce) + ";document.body.append(f);true")
            wait(lambda: evaluate('nested.started'))
            if permissive:
                wait(lambda: all(any('tag=' + tag in uri for uri in nested_requests) for tag in ('iframe', 'object', 'embed')))
            else:
                wait(lambda: evaluate("nested.violations.filter(v=>v.directive==='object-src').length>=2 && nested.violations.some(v=>v.directive==='frame-src')"))
                assert not nested_requests, (path, nested_requests)
            nested.append({'path': path, 'permissive': permissive, **evaluate('nested'), 'requests': list(nested_requests), 'navigation_decisions': list(navigation_decisions)})
    args.report.write_text(json.dumps({'status':'passed','engine':f'{WebKit2.get_major_version()}.{WebKit2.get_minor_version()}.{WebKit2.get_micro_version()}','cases':evidence,'nested':nested}, indent=2))
finally:
    window.destroy()
