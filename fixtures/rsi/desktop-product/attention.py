"""Native question and approval navigation through actual desktop controls."""
import json


def provider_reply(body):
    messages = body.get('messages', [])
    index = next((i for i in range(len(messages)-1, -1, -1) if messages[i]['role']=='user'), -1)
    if index < 0 or any(m['role']=='tool' for m in messages[index+1:]):
        return None
    prompt = str(messages[index].get('content', ''))
    if 'desktop attention question' in prompt:
        name, arguments = 'ask_user', {'questions':[{'id':'choice','prompt':'Desktop attention exact question','options':['Teal','Blue']}]}
    elif 'desktop attention approval' in prompt:
        name, arguments = 'bash', {'command':"printf 'attention-approved\\n'"}
    else:
        return None
    return {'tool_calls':[{'index':0,'id':f'desktop-attention-{index}','type':'function','function':{'name':name,'arguments':json.dumps(arguments)}}]}


def verify(script, button, fill, until, screenshot, report):
    fill('textarea[aria-label="Main message"]', 'desktop attention question')
    button('Send ↗')
    # require_approval covers ask_user itself before its question can be published.
    until(lambda: script(r'return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Review permission 1")'))
    button('Review permission 1')
    button('Allow once')
    until(lambda: script(r'return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Answer question 1")'))
    button('Answer question 1')
    until(lambda: script(r'return document.querySelector("#detail")?.textContent.includes("Desktop attention exact question")'))
    screenshot('attention-native-question.png')
    button('Teal')
    button('Send answers')
    until(lambda: script(r'return !document.querySelector("#detail")?.getBoundingClientRect().height&&document.querySelector(".pane-status")?.textContent==="Completed"'))
    fill('textarea[aria-label="Main message"]', 'desktop attention approval')
    button('Send ↗')
    until(lambda: script(r'return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Review permission 1")'))
    button('Review permission 1')
    until(lambda: script(r'return document.querySelector("#detail")?.textContent.includes("attention-approved")'))
    screenshot('attention-native-approval.png')
    button('Allow once')
    until(lambda: script(r'return document.querySelector(".pane-status")?.textContent==="Completed"'))
    until(lambda: script(r'return ![...document.querySelectorAll(".attention-navigation button")].some(e=>/^(Answer question|Review permission)/.test(e.textContent))'))
    (report / 'attention.json').write_text(json.dumps({'status':'passed','native_question_exact_navigation':True,'native_approval_exact_navigation':True,'settled_requests_removed':True},indent=2))
