"""Opt-in Node Workflow through actual Linux Tauri/WebKit controls."""
import json
import os
import re
from pathlib import Path


def configure(config, host):
    node = os.environ.get('RSI_TEST_NODE')
    assert node and Path(node).is_absolute(), 'explicit absolute RSI_TEST_NODE required'
    host.write_text(re.sub(r'^steps[ \t]*=[ \t]*\[\][ \t]*$', '', host.read_text(), flags=re.MULTILINE))
    with host.open('a') as output:
        output.write('\n[[steps]]\nkind="patch"\ntarget="program-runtime"\nconfig_json='+json.dumps(json.dumps({'node':node}))+'\n[[steps]]\nkind="patch"\ntarget="program-runtime"\nenabled=true\n')
    settings=json.loads((config/'settings.json').read_text())
    settings['rsi.agent-presets']={'default':'workflow'}
    (config/'settings.json').write_text(json.dumps(settings))


def provider_reply(body):
    messages=body.get('messages',[])
    index=next((i for i in range(len(messages)-1,-1,-1) if messages[i]['role']=='user'),-1)
    if index<0 or 'WORKFLOW_DESKTOP' not in str(messages[index].get('content','')): return None
    if any(message['role']=='tool' for message in messages[index+1:]): return None
    script="await workflow.phase('Native Desktop',{pid:process.pid}); return {total:42};"
    return {'tool_calls':[{'index':0,'id':'desktop-workflow','type':'function','function':{'name':'run_workflow','arguments':json.dumps({'script':script,'background':True})}}]}


def resource_button(script, click, until, label):
    item=until(lambda: script('const b=[...document.querySelectorAll(".resource-content button")].find(b=>b.textContent===arguments[0]&&!b.disabled&&b.getBoundingClientRect().width>0&&b.getBoundingClientRect().height>0);if(b)b.scrollIntoView({block:"center"});return b||null',[label]))
    click(item)


def verify(script, button, click, fill, until, screenshot, report):
    button('Verbose')
    fill('textarea[aria-label="Main message"]','WORKFLOW_DESKTOP')
    button('Send')
    until(lambda: script('return document.querySelector(".pane-status")?.textContent==="Completed"&&document.querySelector(".transcript")?.textContent.includes("run_workflow")'))
    button('Toggle resources')
    until(lambda: script('return [...document.querySelectorAll("button")].some(b=>b.textContent==="Workflows")'))
    button('Workflows')
    until(lambda: script('return [...document.querySelectorAll(".resource-content button")].some(b=>b.textContent==="Open workflow")'))
    resource_button(script, click, until, 'Open workflow')
    until(lambda: script('return document.querySelector(".resource-content")?.textContent.includes("Completed")&&[...document.querySelectorAll(".resource-content button")].some(b=>b.textContent==="Read result")'))
    resource_button(script, click, until, 'Read result')
    until(lambda: script('return document.querySelector(".resource-content")?.textContent.includes(\'"total":42\')'))
    screenshot('workflow-result-native.png')
    resource_button(script, click, until, 'Read frozen script')
    until(lambda: script('return document.querySelector(".resource-content")?.textContent.includes("workflow.phase")'))
    screenshot('workflow-script-native.png')
    assert script('const e=document.querySelector(".resource-content");return e.scrollWidth<=e.clientWidth+1')
    (report/'workflow.json').write_text(json.dumps({'ok':True,'actualNode':True,'nativeBridge':True,'result':42,'frozenScript':True},indent=2))
    button('Toggle resources')
    button('Standard')
