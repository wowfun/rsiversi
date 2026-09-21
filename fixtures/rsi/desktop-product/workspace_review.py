"""Actual patch and workspace interval inspection through the native bridge."""
import json

def reply(body):
    messages = body.get('messages', [])
    last = next((i for i in range(len(messages)-1,-1,-1) if messages[i].get('role')=='user'), -1)
    if last < 0 or 'REVIEW_PATCH' not in str(messages[last].get('content')) or any(m.get('role')=='tool' for m in messages[last+1:]): return None
    return {'tool_calls':[{'index':0,'id':'desktop-review-patch','type':'function','function':{'name':'apply_patch','arguments':json.dumps({'patch':'*** Begin Patch\n*** Update File: card.txt\n@@\n-before\n+after · 界\n*** End Patch\n'})}}]}

def verify(script, button, fill, until, screenshot, workspace, report):
    fill('textarea[aria-label="Main message"]','REVIEW_PATCH'); button('Send ↗')
    until(lambda: (workspace / 'card.txt').read_text() == 'after · 界\n')
    until(lambda: script('return document.querySelector(".pane-status")?.textContent==="Completed"'))
    button('Workspace changes'); button('Review workspace changes')
    def ready():
        if script('return [...document.querySelectorAll("dialog")].some(e=>e.textContent.includes("Complete · 1 files"))'): return True
        button('Review workspace changes'); return False
    until(ready); screenshot('review-summary.png')
    # An earlier text-only Turn is also durable; select the interval with one changed file.
    script('''const field=[...document.querySelectorAll("dialog .ui-field")].find(e=>e.textContent.includes("Complete · 1 files"));let e=field.nextElementSibling;while(e&&e.tagName!=="BUTTON")e=e.nextElementSibling;if(!e)throw Error("interval action missing");e.click();return true''')
    button('Open file diff')
    diff = until(lambda: script('return document.querySelector("dialog pre")?.textContent'))
    assert '-before\n' in diff and '+after · 界' in diff and 'committed' not in diff, diff
    screenshot('review-diff.png'); button('Close details')
    (report / 'workspace-review.json').write_text(json.dumps({'status':'passed','native_bridge':True,'dirty_baseline':True,'diff':diff},ensure_ascii=False,indent=2))
