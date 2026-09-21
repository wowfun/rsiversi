"""Source reread and UTF-8 fragment capture through the actual native GUI bridge."""
import json


def verify(script, button, fill, until, screenshot, report):
    source = script(r'return document.querySelector(".pane-session").textContent.split(" · ").at(-1)')
    fill('textarea[aria-label="Main message"]', 'HISTORY_DRAFT_KEEP')
    button('Search history')
    fill('input[aria-label="History conversation ID"]', source)
    fill('input[aria-label="History text query"]', '桌面首次对话')
    button('Search text')
    until(lambda: script(r'return document.querySelector(".history-dialog [role=status]")?.textContent.includes("More indexing needed")'))
    button('Index next batch')
    until(lambda: script(r'return document.querySelector(".history-dialog [role=status]")?.textContent.includes("Caught up")'))
    button('Search text')
    button('Open original')
    original = until(lambda: script(r'return document.querySelector("textarea[aria-label=\"Verified original text\"]")?.value'))
    assert original == '桌面首次对话：请确认收到。', original
    script(r'const e=document.querySelector("textarea[aria-label=\"Verified original text\"]");e.focus();e.setSelectionRange(0,6);return true')
    screenshot('history-original.png')
    button('Freeze selected fragment')
    until(lambda: script(r'return document.querySelector(".history-dialog pre")?.textContent==="桌面首次对话"'))
    screenshot('history-frozen.png')
    button('Add selected reference to draft')
    assert script(r'return document.querySelector("textarea[aria-label=\"Main message\"]").value') == 'HISTORY_DRAFT_KEEP'
    script(r'''window.fixtureHistoryClick=[];for(const type of ['mousedown','mouseup','click'])document.addEventListener(type,event=>{const target=event.target.closest('button');window.fixtureHistoryClick.push({type,label:target?.textContent,x:event.clientX,y:event.clientY,dialog:document.querySelector('dialog[open]')?.getAttribute('aria-label'),draft:document.querySelector('.draft-status')?.textContent});},{capture:true});return true''')
    try:
        button('Preview reference')
        until(lambda: script(r'return [...document.querySelectorAll("dialog pre")].some(e=>e.textContent==="桌面首次对话")'))
    finally:
        events = script('return window.fixtureHistoryClick')
        (report / 'history-reference-click.json').write_text(json.dumps(events, ensure_ascii=False, indent=2))
    assert [(event['type'], event.get('label')) for event in events] == [(kind, 'Preview reference') for kind in ['mousedown', 'mouseup', 'click']], events
    screenshot('history-draft-reference.png')
    button('Close')
    (report / 'history.json').write_text(json.dumps({'status':'passed','source':source,'selected':'桌面首次对话','draft_preserved':True,'native_bridge':True},ensure_ascii=False,indent=2))
