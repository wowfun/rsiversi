"""Native pointer and keyboard gestures over an existing live terminal host."""
import json
from native_pointer import drag
from workbench_geometry import verify as verify_geometry, wait_terminal_fit


def verify(script, button, until, screenshot, call, root, eid, report):
    def pointer(x, y, dx, dy, mouse_button=0):
        origin=script('return {x:screenX,y:screenY+outerHeight-innerHeight}')
        drag(origin['x']+x, origin['y']+y, dx, dy, 3 if mouse_button==2 else 1)

    def rect(selector):
        return script('return document.querySelector(arguments[0])?.getBoundingClientRect().toJSON()', [selector])

    script('window.dockPointer=[];for(const type of ["pointerdown","pointerup","pointercancel","mousedown","mouseup"])document.addEventListener(type,e=>window.dockPointer.push({type,trusted:e.isTrusted,x:e.clientX,y:e.clientY,target:e.target.outerHTML.slice(0,512)}),true);return true')
    before = script('return window.fixtureTerminalCommands.filter(x=>["create","attach","detach"].includes(x.type))')
    terminal = until(lambda: script('return [...document.querySelectorAll("[data-dockkit-tab]")].find(e=>e.textContent.includes("Terminal"))'))
    identity = script('return arguments[0].dataset.dockkitTab', [terminal])
    button('Split resources')
    until(lambda: script('return Boolean(document.querySelector("[data-dockkit-divider]"))'))
    box = rect('[data-dockkit-tab="'+identity+'"]')
    pointer(box['x']+box['width']/2, box['y']+box['height']/2, 0, 0, 2)
    button('Float')
    until(lambda: script('return Boolean(document.querySelector("[data-dockkit-float]"))'))
    floating = rect('[data-dockkit-float]')
    pointer(floating['x']+100, floating['y']+15, 40, 35)
    (report/'dock-pointer.json').write_text(json.dumps({'before':floating,'after':rect('[data-dockkit-float]'),'events':script('return window.dockPointer')},indent=2))
    until(lambda: abs(rect('[data-dockkit-float]')['x']-floating['x']-40) <= 2)
    moved = rect('[data-dockkit-float]')
    handle = rect('[data-dockkit-float-resize]')
    pointer(handle['x']+handle['width']/2, handle['y']+handle['height']/2, 45, 35)
    until(lambda: abs(rect('[data-dockkit-float]')['width']-moved['width']-45) <= 2)
    resized = rect('[data-dockkit-float]')
    assert abs(resized['height']-moved['height']-35) <= 2
    screenshot('dock-terminal-float.png')
    wait_terminal_fit(script, until)
    script('window.fixtureDockPaint=false;requestAnimationFrame(()=>requestAnimationFrame(()=>window.fixtureDockPaint=true));return true')
    until(lambda: script('return window.fixtureDockPaint'))
    screenshot('dock-terminal-float-settled.png')
    button('Dock resource')
    until(lambda: script('return !document.querySelector("[data-dockkit-float]")'))
    assert script('return window.fixtureTerminalCommands.filter(x=>["create","attach","detach"].includes(x.type))') == before
    terminal = until(lambda: script('return document.querySelector(arguments[0])', ['[data-dockkit-tab="'+identity+'"]']))
    call('POST', root + f'/element/{eid(terminal)}/value', {'text': '\ue009z\ue000'})
    until(lambda: script('return Boolean(document.querySelector("[data-dockkit-float]"))'))
    button('Redo layout')
    until(lambda: script('return !document.querySelector("[data-dockkit-float]")'))
    button('Resource fullscreen')
    until(lambda: rect('.resource-dock:not([hidden])')['width'] == script('return innerWidth'))
    button('Exit resource fullscreen')
    assert script('return document.querySelector(".terminal-authority")?.textContent') == 'You have control'
    (report/'dock.json').write_text(json.dumps({'pointerMove':moved,'pointerResize':resized,
        'keyboardUndo':True,'redo':True,'fullscreen':True,'terminalFollowerPreserved':True},indent=2))
    verify_geometry(script,button,until,screenshot,call,root,report)
