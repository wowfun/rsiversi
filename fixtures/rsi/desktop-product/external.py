"""External ACP controls through actual WebKitGTK elements and the native bridge."""
import json
import os
from pathlib import Path
import shutil


def configure(profile, workspace, state):
    node = shutil.which('node')
    assert node, 'Independent ACP SDK fixture requires an explicit Node executable'
    agent = Path(__file__).resolve().parents[1] / 'acp/agent.mjs'
    q = json.dumps
    profile.write_text(profile.read_text().replace('steps = []', ''))
    with profile.open('a') as out:
        out.write(f'''\n[[steps]]
kind='patch'
target='rsi-acp'
[steps.config]
directory={q(str(state / 'acp'))}
[[steps.config.endpoints]]
id='sdk-agent'
enabled=true
cwd={q(str(workspace))}
sandbox='danger-full-access'
[steps.config.endpoints.launch]
program={q(node)}
arguments=[{q(str(agent))},{q(str(workspace))}]
[steps.config.endpoints.launch.environment]
FIXTURE_SECRET={{kind='literal',value='private-fixture-secret'}}
[[steps.config.endpoints.mcp_servers]]
name='fixture-mcp'
[steps.config.endpoints.mcp_servers.launch]
program='/usr/bin/python3'
[steps.config.endpoints.mcp_servers.launch.environment]
MCP_SECRET={{kind='literal',value='private-fixture-secret'}}
''')


def verify(script, button, fill, until, screenshot, workspace, report):
    button('Start sdk-agent')
    until(lambda: script('return !!document.querySelector(".external-composer")'))
    fill('[aria-label="External message"]', 'permission')
    button('Send ↗')
    until(lambda: script('return document.querySelectorAll(".external-permission button").length===4'))
    until(lambda: script('return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Review permission 1")'))
    button('Review permission 1')
    until(lambda: script('return document.activeElement?.classList.contains("external-permission")'))
    screenshot('external-permission.png')
    button('Always · allow always')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("completed")'))
    until(lambda: script('return document.querySelector(".external-transcript")?.textContent.includes("SDK_AGENT_VERIFIED")'))
    assert script('return window.externalExecuted===undefined')
    screenshot('external-conversation.png')
    fill('[aria-label="External message"]', 'reject-always')
    button('Send ↗')
    until(lambda: script('return document.querySelectorAll(".external-permission button").length===4'))
    button('Never · reject always')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("completed")'))
    fill('[aria-label="External message"]', 'wait')
    button('Send ↗')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("running")'))
    button('Cancel prompt')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("cancelled")'))
    button('Close peer')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("Closed")'))
    button('Reload remote history')
    until(lambda: script('return [...document.querySelectorAll(".external-record pre")].some(e=>e.textContent==="1199")'),45)
    assert script('return document.querySelectorAll(".external-record").length') == 128
    screenshot('external-replay.png')
    button('Close peer')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("Closed")'))
    pids = (workspace / 'peer-pids').read_text().splitlines()
    for pid in pids:
        try:
            os.kill(int(pid), 0)
        except ProcessLookupError:
            continue
        raise AssertionError(f'ACP peer {pid} was not reaped')
    (report / 'external.json').write_text(json.dumps({'sdk':'1.4.0','permission_options':4,'permission_requested_again':True,'cancel':True,'replay':1200,'retained':128,'peers_reaped':len(pids)},indent=2))


def provider_reply(body):
    messages = body.get('messages', [])
    index = next((i for i in range(len(messages)-1, -1, -1) if messages[i]['role']=='user'), -1)
    if index < 0 or 'start an external delegation' not in str(messages[index].get('content', '')):
        return None
    if any(m['role']=='tool' for m in messages[index+1:]):
        return None
    return {'tool_calls':[{'index':0,'id':'desktop-delegate','type':'function','function':{'name':'external_agent','arguments':json.dumps({'operation':'start','endpoint':'sdk-agent'})}}]}


def delegation(script, button, fill, until, screenshot, workspace, report):
    native_session = script('return document.querySelector(".pane-session").title')
    before = (workspace / 'new-count').read_text()
    fill('textarea[aria-label="Main message"]', 'Please start an external delegation')
    button('Send ↗')
    until(lambda: script('return [...document.querySelectorAll(".transcript button")].some(b=>b.textContent==="Open external conversation")'))
    until(lambda: script('return document.querySelector(".pane-status")?.textContent.includes("Completed")'))
    screenshot('delegation-card.png')
    button('Open external conversation')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("ready")'))
    assert (workspace / 'new-count').read_text() == before + 'new\n'
    fill('[aria-label="External message"]', 'work')
    button('Send ↗')
    until(lambda: script('return document.querySelectorAll(".external-permission button").length===4'))
    button('Always · allow always')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("completed")'))
    screenshot('delegation-conversation.png')
    button('Close peer')
    until(lambda: script('return document.querySelector(".external-status")?.textContent.includes("Closed")'))
    for pid in (workspace / 'peer-pids').read_text().splitlines():
        try:
            os.kill(int(pid), 0)
        except ProcessLookupError:
            continue
        raise AssertionError('delegated peer was not reaped')
    script('const session=[...document.querySelectorAll("#sessions .session-row button")].find(e=>e.title===arguments[0]);if(!session)throw new Error("Original native Session is absent");session.click();return true', [native_session])
    until(lambda: script(r'return document.querySelector(".pane-session")?.title===arguments[0]&&!!document.querySelector("textarea[aria-label=\"Main message\"]")', [native_session]))
    (report / 'delegation.json').write_text(json.dumps({'same_conversation':True,'one_start':True,'reaped':True,'returned_native_session':native_session}))
