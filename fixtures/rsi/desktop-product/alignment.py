"""Shared GUI geometry, directory authority and native clipboard in Linux WebKitGTK."""
import json


def verify(script, button, fill, until, screenshot, call, root, report, workspace):
    evidence=[]
    button('Copy code')
    until(lambda: script('return [...document.querySelectorAll(".copy-code")].some(button=>button.textContent==="Copied")'))
    evidence.append({'nativeClipboard':True})
    button('Choose workspace')
    until(lambda: script('return Boolean(document.querySelector(".directory-column"))'))
    button('Edit directory path')
    fill('[aria-label="Directory path"]',str(workspace))
    button('Go')
    until(lambda: script('return document.querySelector(".directory-column:last-child")?.getAttribute("aria-label")===arguments[0]',[str(workspace)]))
    button('+ New folder')
    fill('.directory-create input','native-folder')
    button('Create folder')
    until(lambda: script('return document.querySelector(".directory-column:last-child")?.getAttribute("aria-label")?.endsWith("/native-folder")'))
    assert (workspace/'native-folder').is_dir()
    screenshot('directory.png')
    button('Cancel')
    until(lambda: script('return !document.querySelector(".directory-picker")'))
    evidence.append({'directoryCreate':True})
    for width in (390,768,1024,1440,1920):
        call('POST',root+'/window/rect',{'width':width,'height':900})
        until(lambda: script('return innerWidth')==width)
        for theme in ('light','dark','system'):
            button('Commands');fill('[aria-label="Search commands"]','appearance');button('Appearance and input preferences')
            until(lambda:script('return Boolean(document.querySelector("select[aria-label=\\"Settings / appearance / theme\\"]"))'))
            script('const e=document.querySelector("select[aria-label=\\"Settings / appearance / theme\\"]");e.value=[...e.options].find(o=>o.textContent===arguments[0]).value;e.dispatchEvent(new Event("change",{bubbles:true}));return true',[theme])
            button('Save settings');until(lambda:script('return document.documentElement.dataset.theme===arguments[0]',[theme]));button('Close details')
            geometry=script('const c=document.querySelector(".pane.selected .composer").getBoundingClientRect(),b=document.querySelector(".pane.selected [data-testid=\\"composer-send\\"]"),r=b.getBoundingClientRect();return {width:innerWidth,theme:document.documentElement.dataset.theme,scheme:getComputedStyle(document.documentElement).colorScheme,composer:c.toJSON(),overflow:document.documentElement.scrollWidth-innerWidth,sendHit:b.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2))}')
            assert geometry['overflow']<=1 and geometry['sendHit'] and geometry['composer']['x']>=0,geometry
            evidence.append(geometry);screenshot(f'layout-{width}-{theme}.png')
    call('POST',root+'/window/rect',{'width':1440,'height':980})
    until(lambda:script('return innerWidth')==1440)
    (report/'alignment.json').write_text(json.dumps(evidence,indent=2))
