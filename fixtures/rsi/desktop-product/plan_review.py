"""Closed plan review through the Linux Tauri bridge and actual WebKit input."""
import json

requests = []


def provider_reply(body):
    messages = body.get('messages', [])
    index = next((i for i in range(len(messages)-1, -1, -1) if messages[i]['role'] == 'user'), -1)
    if index < 0 or 'PLAN_REVIEW_DESKTOP' not in str(messages[index].get('content', '')):
        return None
    requests.append(body)
    results = {m.get('tool_call_id'): m for m in messages[index+1:] if m['role'] == 'tool'}
    if 'review' in results:
        return None
    if 'save' in results:
        saved = json.loads(results['save']['content'])
        identity, name, arguments = 'review', 'request_plan_execution', {'plan_ref': saved['plan_ref']}
    else:
        identity, name, arguments = 'save', 'plan_write', {'title': 'Native Desktop plan', 'body': 'Inspect source and verify the isolated change.\n<em>Literal plan text</em>'}
    return {'tool_calls': [{'index': 0, 'id': identity, 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(arguments)}}]}


def verify(script, button, fill, until, screenshot, report):
    button('Trajectory')
    fill('textarea[aria-label="Main message"]', '/plan on')
    button('Send ↗')
    until(lambda: script('return document.querySelector(".command-receipt")?.textContent.includes("Committed")'))
    until(lambda: script("return document.querySelector('textarea').value === ''"))
    fill('textarea[aria-label="Main message"]', 'PLAN_REVIEW_DESKTOP')
    button('Send ↗')
    until(lambda: script('return [...document.querySelectorAll(".attention-navigation button")].some(e=>e.textContent==="Answer question 1")'))
    button('Answer question 1')
    until(lambda: script('return document.querySelector("#detail .review-plan")'))
    assert script('return document.querySelector("#detail .review-plan").textContent.includes("<em>Literal plan text</em>")')
    assert not script('return document.querySelector("#detail .review-plan em")')
    assert script('return ["Approve and execute","Request changes","Decline and end turn"].every(label=>[...document.querySelectorAll("#detail button")].some(b=>b.textContent===label&&!b.disabled))')
    assert script('const e=document.querySelector("#detail");return e.scrollWidth<=e.clientWidth+1')
    screenshot('plan-review-native.png')
    fill('textarea[aria-label="Optional review feedback"]', 'Verified through native WebKit input')
    button('Approve and execute')
    until(lambda: script('return !document.querySelector("#detail").open'))
    until(lambda: len(requests) == 3 and script('return document.querySelector(".pane-status")?.textContent==="Completed"'))
    assert 'Plan mode is disabled' in json.dumps(requests[2]['messages'])
    assert 'Verified through native WebKit input' in json.dumps(requests[2]['messages'])
    screenshot('plan-approved-native.png')
    (report / 'plan-review.json').write_text(json.dumps({'ok': True, 'native_bridge': True, 'literal_plan': True, 'closed_choices': True, 'feedback': True, 'approval_visible_to_next_model': True, 'mock_requests': 3}, indent=2))
    (report / 'plan-review-requests.json').write_text(json.dumps(requests, indent=2))
    button('Chat')
