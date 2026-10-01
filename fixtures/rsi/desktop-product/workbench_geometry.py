"""WebKitGTK geometry is measured independently of Chromium pixel baselines."""
import json
from presentation import open_appearance


def wait_terminal_fit(script, until):
    until(lambda: script(r'const h=document.querySelector(".terminal-screen"),t=h?.querySelector(".xterm-screen");if(!h||!t)return false;const a=h.getBoundingClientRect(),b=t.getBoundingClientRect();return a.width>0&&a.height>0&&b.width>0&&b.height>0&&b.width<=a.width+2&&a.width-b.width<25&&b.height<=a.height+2&&a.height-b.height<35'))


def verify(script, button, until, screenshot, call, root, report):
    evidence=[]
    for theme in ('light','dark'):
        call('POST',root+'/window/rect',{'width':1440,'height':900})
        until(lambda: script(r'return innerWidth===1440'))
        open_appearance(script, until, lambda: button('Appearance'))
        script(r'const e=document.querySelector("[aria-label=\"Settings / appearance / theme\"]");e.value=[...e.options].find(o=>o.textContent===arguments[0]).value;e.dispatchEvent(new Event("change",{bubbles:true}));return true',[theme])
        button('Save settings')
        until(lambda: script(r'return document.documentElement.dataset.theme===arguments[0]',[theme]))
        button('Close details')
        for width,height in ((1440,900),(1024,768),(767,900),(390,844)):
            call('POST',root+'/window/rect',{'width':width,'height':height})
            until(lambda: script(r'return innerWidth===arguments[0]&&innerHeight===arguments[1]',[width,height]))
            # Native window resize precedes React's media-query update and the
            # terminal's debounced, acknowledged resize. Measure the settled view.
            until(lambda: script(r'return document.querySelector(".resource-dock").classList.contains("resource-fullscreen")===arguments[0]',[width<768]))
            if script(r'return document.querySelector("[aria-label=\"Toggle resources\"]").getAttribute("aria-expanded")!=="true"'):
                button('Toggle resources')
            until(lambda: script(r'const d=document.querySelector(".resource-dock:not([hidden])")?.getBoundingClientRect();return d&&Math.abs(d.top)<=2&&Math.abs(d.height-innerHeight)<=2'))
            wait_terminal_fit(script, until)
            geometry=script(r'const rect=s=>document.querySelector(s)?.getBoundingClientRect().toJSON();return {width:innerWidth,height:innerHeight,overflow:document.documentElement.scrollWidth-innerWidth,dock:rect(".resource-dock:not([hidden])"),sidebar:rect(".workbench>.sidebar,.navigation-rail"),main:rect(".workspace-main")}')
            assert geometry['overflow']<=1,geometry
            assert abs(geometry['dock']['height']-height)<=2 and abs(geometry['dock']['top'])<=2,geometry
            if width<768: assert abs(geometry['dock']['width']-width)<=2,geometry
            else:
                assert geometry['main']['width']>=398,geometry
                expected=min(width*.45,width-geometry['sidebar']['width']-400)
                assert abs(geometry['dock']['width']-expected)<=2,geometry
                header=script(r'const controls=[...document.querySelectorAll(".session-toolbar .pane-tabs button,.conversation-menu>summary,.connected-header button")].filter(e=>e.getClientRects().length&&!e.hidden);return controls.map(e=>{const r=e.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return {label:e.getAttribute("aria-label")||e.textContent,hit:!!hit&&(e===hit||e.contains(hit))}})')
                assert header and all(e['hit'] for e in header),header
                geometry['headerControls']=header
            screenshot(f'workbench-{width}-{theme}.png')
            assert script(r'return [...document.querySelectorAll(".terminal-notice")].every(e=>!e.textContent.trim())'), 'Terminal geometry produced an error'
            button('Hide resources')
            if width<768: button('Toggle navigation')
            button('Settings')
            until(lambda: script(r'return !!document.querySelector(".settings-modal")'))
            settings=script(r'const e=document.querySelector(".settings-modal"),n=e.querySelector(".settings-tabs");return {box:e.getBoundingClientRect().toJSON(),navigation:n?.getBoundingClientRect().toJSON(),overflow:e.scrollWidth-e.clientWidth}')
            assert settings['overflow']<=1,settings
            if width<768:
                assert abs(settings['box']['width']-width)<=2 and abs(settings['box']['height']-height)<=2,settings
            else:
                assert abs(settings['box']['width']-800)<=2,settings
                assert abs(settings['box']['height']-min(800,height-48))<=2,settings
                assert abs(settings['navigation']['width']-188)<=2,settings
            screenshot(f'settings-{width}-{theme}.png')
            button('Close settings');button('Toggle resources')
            evidence.append({'theme':theme,**geometry,'settings':settings})
    call('POST',root+'/window/rect',{'width':1440,'height':980})
    until(lambda: script(r'return innerWidth===1440'))
    sizes=script(r'return window.fixtureTerminalCommands.filter(e=>e.type==="resize").map(e=>e.size)')
    assert sizes and all(isinstance(s['rows'],int) and 1<=s['rows']<=200 and isinstance(s['columns'],int) and 1<=s['columns']<=500 for s in sizes),sizes
    (report/'workbench-geometry.json').write_text(json.dumps(evidence,indent=2))
