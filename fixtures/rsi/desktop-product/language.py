"""Native service UI with a pinned real rust-analyzer; no inference queries."""
import json
from pathlib import Path

def verify(script,button,fill,until,screenshot,report,position,workspace):
    resource = '.resource-dock:not([hidden]) [data-dockkit-content]:not([aria-hidden=true]) .resource-content'
    def read(code, args=None):
        return script(f'const resource=document.querySelector({json.dumps(resource)});'+code, args or [])
    def repeat_indexing_read():
        if not read('return [...resource.querySelectorAll("button")].some(e=>e.textContent==="Repeat query"&&!e.disabled)'):
            return False
        error = read('return [...resource.querySelectorAll(".ui-text")].map(e=>e.textContent).find(t=>t.startsWith("Language read failed:"))')
        if error and error != 'Language read failed: language server rejected request (code -32801)':
            raise AssertionError(error)
        read('const e=[...resource.querySelectorAll("button")].find(e=>e.textContent==="Repeat query");if(e&&!e.disabled)e.click();return true')
        return False
    button('Service extensions');button('Code intelligence')
    fill(resource+' input[aria-label="Line"]',str(position['line']));fill(resource+' input[aria-label="Column"]',str(position['column']))
    button('Find definition')
    def ready():
        if read('return [...resource.querySelectorAll("button")].some(e=>e.textContent==="Open src/main.rs:2 (UTF-16 column 12)")'): return True
        return repeat_indexing_read()
    until(ready);button('Open src/main.rs:2 (UTF-16 column 12)')
    until(lambda: read('return resource?.textContent.includes(arguments[0])',[position['definition']]))
    source=read('return resource.querySelector("pre")?.textContent');assert source.startswith('pub struct Bird;'),source
    screenshot('language-definition.png')
    button('New language query');fill(resource+' input[aria-label="Line"]',str(position['line']));fill(resource+' input[aria-label="Column"]',str(position['column']));button('Read hover')
    until(lambda: read('return resource.querySelector("pre")?.textContent.includes("Bird")') or repeat_indexing_read());screenshot('language-hover.png')
    button('New language query');fill(resource+' input[aria-label="Line"]',str(position['line']));fill(resource+' input[aria-label="Column"]',str(position['column']));button('Find references')
    def references_ready():
        if read('return [...resource.querySelectorAll("button")].some(e=>e.textContent==="More locations")'): return True
        return repeat_indexing_read()
    until(references_ready)
    labels='return [...resource.querySelectorAll("button")].map(e=>e.textContent).filter(s=>s.startsWith("Open "))'
    first=read(labels);assert len(first)==16,first
    source_path=Path(workspace)/'src/main.rs';original=source_path.read_text()
    try:
        source_path.write_text('x'*(1024*1024+1))
        button('More locations')
        until(lambda: len(current:=read(labels))==16 and not set(current)&set(first))
        screenshot('language-cached-page.png')
        button('Repeat query')
        until(lambda: read('return resource?.textContent.includes("Language read failed:")'))
        screenshot('language-repeat-error.png')
    finally:
        source_path.write_text(original)
    read('window.fixtureLanguageError=resource.querySelector(".ui-text");return true')
    button('Repeat query')
    until(lambda: script('return !window.fixtureLanguageError.isConnected'))
    script('delete window.fixtureLanguageError;return true')
    until(references_ready);assert len(read(labels))==16;button('Close details')
    (report/'language.json').write_text(json.dumps({'status':'passed','native_bridge':True,'server':position['version'],'location':position['definition'],'source':source,'cached_page_survives_source_replacement':True,'explicit_repeat_revalidates_source':True},ensure_ascii=False,indent=2))
