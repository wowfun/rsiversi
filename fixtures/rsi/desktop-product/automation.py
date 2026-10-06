"""Automation projection and mutation through the actual native API/Worker family."""
import json
import re


def configure(host, seed):
    runtime = json.loads((seed / 'runtime-config.json').read_text())
    host.write_text(re.sub(r'^steps[ \t]*=[ \t]*\[\][ \t]*$', '', host.read_text(), flags=re.MULTILINE))
    with host.open('a') as output:
        output.write('\n[[steps]]\nkind="patch"\ntarget="automation"\nconfig_json=' + json.dumps(json.dumps({'directory': str(seed.resolve()), 'runtime': runtime})) + '\n[[steps]]\nkind="patch"\ntarget="automation"\nenabled=true\n')


def verify(script, button, click, until, screenshot, report):
    script(r'''window.fixtureAutomation={attempts:{},requests:[],frames:0,mutation:null};
const original=window.fetch;window.fetch=(path,options)=>{
 const route=String(path);let tracked;
 if(route==='/_call/command'&&typeof options?.body==='string'){
  const body=JSON.parse(options.body);if(body.action==='automation')tracked={route,request:body.request};
 }else if(route==='/_call/automation_artifact')tracked={route};
 if(tracked)window.fixtureAutomation.requests.push(tracked);
 return original(path,options).then(response=>{
  if(tracked)tracked.status=response.status;
  if(route.startsWith('/_frame'))void response.clone().json().then(frame=>{
   const automation=frame.view?.view?.automation??frame.view?.sections?.automation;
   if(automation){window.fixtureAutomation.frames++;
    if(automation.attempt){window.fixtureAutomation.attempts[automation.attempt.id]=automation.attempt;window.fixtureAutomation.currentAttempt=automation.attempt;}
    if(automation.mutation)window.fixtureAutomation.mutation=automation.mutation;
   }
  });return response;
 });};return true''')
    button('Deployment checks')
    until(lambda: script('return document.querySelector(".automation-panel")?.textContent.includes("assertion failed")'))
    until(lambda: script('const b=document.querySelector(".automation-row:not(:disabled)"); return !!b&&b.getBoundingClientRect().height>0'))
    click(".automation-row:not(:disabled)")
    until(lambda: script('return document.querySelector(".automation-detail")?.textContent.includes("Expected visible text is missing")'))
    button('Read screenshot')
    until(lambda: script('const i=document.querySelector(".automation-screenshot");return !!i&&i.complete&&i.naturalWidth===1280'))
    assert script('const p=document.querySelector(".automation-panel");return p.scrollWidth<=p.clientWidth+1')
    original = until(lambda: script('return Object.values(window.fixtureAutomation.attempts).find(a=>a.result?.outcome==="assertion_failed")'))
    image = script('const i=document.querySelector(".automation-screenshot");return {width:i.naturalWidth,height:i.naturalHeight}')
    screenshot('automation-check-native.png')
    button('New attempt')
    until(lambda: script('return document.querySelector(".automation-panel")?.textContent.includes("Attempt 2: queued")'))
    receipt = until(lambda: script('return window.fixtureAutomation.mutation'))
    assert receipt['id'] != original['id']
    button('Read attempt')
    until(lambda: script('return document.querySelector(".automation-detail h3")?.textContent==="Attempt 2"'))
    def settled():
        if script('return document.querySelector(".automation-detail")?.textContent.includes("policy blocked")'):
            return True
        button('Read attempt')
        return False
    until(settled, 60)
    screenshot('automation-new-attempt-native.png')
    current = until(lambda: script('return window.fixtureAutomation.attempts[arguments[0]]?.result?.outcome==="policy_blocked"?window.fixtureAutomation.attempts[arguments[0]]:null', [receipt['id']]))
    button('Refresh')
    until(lambda: script('return !document.querySelector(".automation-row")?.disabled'))
    row = script('const e=[...document.querySelectorAll(".automation-row")].find(e=>e.textContent.includes("Attempt "+arguments[0]+" ·"));return e?".automation-list > :nth-child("+([...e.parentElement.children].indexOf(e)+1)+")":null', [original['id']])
    assert row
    click(row)
    until(lambda: script('return document.querySelector(".automation-detail h3")?.textContent===arguments[0]', ['Attempt ' + original['id']]))
    reread = until(lambda: script('const a=window.fixtureAutomation.currentAttempt;return a?.id===arguments[0]?a:null', [original['id']]))
    assert reread['result'] == original['result']
    transport = script('return {origin:location.origin,nativeBridge:typeof window.__TAURI_INTERNALS__?.invoke==="function",...window.fixtureAutomation}')
    assert transport['nativeBridge'] and transport['frames'] > 0
    assert all(request.get('status') == 200 for request in transport['requests'])
    assert any(request.get('request',{}).get('operation') == 'resume' for request in transport['requests'])
    (report / 'automation.json').write_text(json.dumps({'ok': True, 'transport': transport, 'image': image, 'mutation': receipt, 'originalAttempt': reread, 'newAttempt': current}, indent=2))
    # Close the actual modal using its button, without dispatching another mutation.
    button('Close')
