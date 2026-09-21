"""Recorded result declaration read from the durable Tool contribution baseline."""
import json


def provider_reply(body):
    messages = body.get('messages', [])
    index = next((i for i in range(len(messages)-1, -1, -1) if messages[i]['role']=='user'), -1)
    if index < 0 or 'desktop typed profile catalog' not in str(messages[index].get('content', '')) or any(m['role']=='tool' for m in messages[index+1:]):
        return None
    return {'tool_calls':[{'index':0,'id':'desktop-typed','type':'function','function':{'name':'host_profile','arguments':json.dumps({'operation':'catalog'})}}]}


def verify(script, button, click, fill, until, screenshot, report):
    button('Trajectory')
    fill('textarea[aria-label="Main message"]', 'desktop typed profile catalog')
    button('Send ↗')
    answered = False
    def completed():
        nonlocal answered
        if not answered and script(r'return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Review permission 1")'):
            button('Review permission 1')
            button('Allow once')
            answered = True
        return script(r'return document.querySelector(".pane-status")?.textContent==="Completed"&&[...document.querySelectorAll(".message.tool")].some(e=>e.textContent.includes("host_profile"))')
    until(completed)
    click(script(r'return [...document.querySelectorAll(".message.tool")].at(-1)?.querySelector("button")'))
    button('Recorded result')
    until(lambda: script(r'return document.querySelector("#detail .ui-contribution")?.textContent.includes("rsi.profile-leaves · version 1")'))
    assert script(r'return document.querySelector("#detail .ui-contribution").textContent.includes("catalog")')
    assert script(r'const e=document.querySelector("#detail .ui-contribution");return e.scrollWidth<=e.clientWidth+1'), 'recorded metadata overflows its card'
    assert script(r'const e=document.querySelector("#detail");return e.scrollWidth<=e.clientWidth+1'), 'recorded metadata overflows its dialog'
    screenshot('typed-recorded-result.png')
    button('Close details')
    button('Chat')
    (report / 'typed-results.json').write_text(json.dumps({'status':'passed','tool':'host_profile','recorded_contract':'rsi.profile-leaves','version':1,'authority':'agent catalog read only','native_bridge':True},indent=2))
