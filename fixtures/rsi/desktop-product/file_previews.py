"""Actual native Files sources and opaque HTML frame, with WebDriver input."""
import json
from preview_policy import verify as verify_policy

def verify(script, button, fill, until, screenshot, frame, workspace, report):
    directory = workspace / 'previews'; directory.mkdir()
    svg = '<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320"><rect width="640" height="320" fill="#0b716c"/><text x="30" y="160" font-size="36" fill="white">Linux workspace preview</text></svg>'
    (directory / 'diagram.svg').write_text(svg)
    (directory / 'sample.rs').write_text('// Native preview · 界\nfn main() { println!("hello"); }\n')
    (directory / 'report.md').write_text('# Native report\n\n| Result | Status |\n|---|---|\n| WebKit | Ready |\n\n```rust\nfn answer() -> u8 { 42 }\n```\n\n![diagram](diagram.svg)\n\n<script>parent.markdownExecuted=true</script>')
    (directory / 'colors.css').write_text('body { background: rgb(235, 247, 241); color: rgb(10, 80, 70); }')
    (directory / 'theme.css').write_text('@import "colors.css"; body { padding:24px; font-family:sans-serif; }')
    (directory / 'app.js').write_text("document.querySelector('#loaded').textContent='Local script loaded';let n=0;document.querySelector('#counter').onclick=()=>document.querySelector('#count').textContent=String(++n);try{parent.document.body.dataset.previewEscaped='yes'}catch{document.querySelector('#isolated').textContent='Parent isolated'}document.querySelector('#bridge').textContent=typeof window.__TAURI_INTERNALS__;document.addEventListener('securitypolicyviolation',e=>{if(e.effectiveDirective==='connect-src')document.querySelector('#network').dataset.blocked='connect-src'});fetch('https://preview-fixture.invalid/probe').catch(()=>document.querySelector('#network').textContent='Network blocked');")
    (directory / 'demo.html').write_text('<!doctype html><html><head><link rel="stylesheet" href="theme.css"></head><body><h1>Interactive native preview</h1><p id="loaded"></p><button id="counter">Count: <span id="count">0</span></button><p id="isolated"></p><p id="bridge"></p><p id="network"></p><img src="diagram.svg" width="300"><script src="app.js"></script></body></html>')
    def open_file(name):
        if script('return document.querySelector("#detail")?.open'): button('Close details')
        button('Workspace files')
        fill('input[aria-label="Workspace-relative path"]', 'previews/' + name)
        button('Read file')
        until(lambda: script('return !!document.querySelector(".file-preview")'))
    (directory / 'missing.html').write_text((directory / 'demo.html').read_text().replace('<h1>Interactive native preview</h1>', '<h1>Missing resources preserve HTML</h1><img alt="Missing image" src="missing.png"><link rel="stylesheet" href="missing.css"><script src="missing.js"></script>'))
    open_file('missing.html')
    until(lambda: script('return document.querySelector("iframe.file-html")'))
    frame(script('return document.querySelector("iframe.file-html")'))
    try:
        until(lambda: script('return document.querySelector("#loaded")?.textContent==="Local script loaded"'))
        assert script('return document.querySelector("h1").textContent') == 'Missing resources preserve HTML'
        assert script('return document.querySelector("img[alt]").getAttribute("src")') == 'about:blank'
        button('Count: 0'); assert script('return document.querySelector("#count").textContent') == '1'
    finally: frame(None)
    assert script('return document.querySelector(".file-preview").textContent.includes("missing.png")')
    screenshot('preview-missing.png')
    open_file('sample.rs')
    until(lambda: script('return document.querySelector(".file-code")?.textContent.includes("fn main")'))
    assert script('return document.querySelectorAll(".file-token[style]").length') > 0
    button('Wrap lines'); screenshot('preview-code.png')
    open_file('report.md')
    until(lambda: script('const img=document.querySelector(".file-markdown img");return document.querySelector(".file-markdown table")&&img?.complete&&img.naturalWidth===640'))
    assert not script('return Boolean(window.markdownExecuted)')
    screenshot('preview-markdown.png'); button('Source')
    until(lambda: script('return document.querySelector(".file-code")?.textContent.includes("![diagram]")'))
    (directory / 'report.md').write_text('# Explicit refresh\n')
    button('Preview'); until(lambda: script('return document.querySelector(".file-markdown h1")?.textContent==="Native report"'))
    button('Refresh'); until(lambda: script('return document.querySelector(".file-markdown h1")?.textContent==="Explicit refresh"'))
    open_file('diagram.svg');until(lambda: script('const img=document.querySelector(".file-image");return img?.complete&&img.naturalWidth===640'))
    button('100%');assert script('return document.querySelector(".file-image").style.width') == '640px'
    button('Fit');screenshot('preview-image.png')
    open_file('demo.html');until(lambda: script('return document.querySelector("iframe.file-html")'))
    frame(script('return document.querySelector("iframe.file-html")'))
    try:
        until(lambda: script('return document.querySelector("#loaded")?.textContent==="Local script loaded"'))
        assert script('return document.querySelector("#isolated").textContent') == 'Parent isolated'
        assert script('return document.querySelector("#bridge").textContent') == 'undefined'
        assert script('return getComputedStyle(document.body).backgroundColor') == 'rgb(235, 247, 241)'
        until(lambda: script('return document.querySelector("#network").textContent==="Network blocked" && document.querySelector("#network").dataset.blocked==="connect-src"'))
        button('Count: 0');assert script('return document.querySelector("#count").textContent') == '1'
        script("""window.nestedViolations=[];document.addEventListener('securitypolicyviolation',e=>nestedViolations.push({directive:e.effectiveDirective,blocked:e.blockedURI}));for(const tag of ['iframe','object','embed']){const element=document.createElement(tag),url='rsi://foreign/nested-target';if(tag==='object'){element.data=url;element.type='text/html'}else{element.src=url;if(tag==='embed')element.type='text/html'}document.body.append(element)}return true""")
        until(lambda: script("return nestedViolations.some(v=>v.directive==='frame-src') && nestedViolations.filter(v=>v.directive==='object-src').length===2"))
        (report / 'nested-preview-policy.json').write_text(json.dumps(script('return nestedViolations'), indent=2))
    finally: frame(None)
    assert not script('return !!document.body.dataset.previewEscaped')
    screenshot('preview-html.png');button('Source');assert not script('return !!document.querySelector("iframe.file-html")')
    button('Preview');until(lambda: script('return document.querySelector("iframe.file-html")'))
    frame(script('return document.querySelector("iframe.file-html")'))
    try: until(lambda: script('return document.querySelector("#count")?.textContent==="0"'))
    finally: frame(None)
    button('Close details')
    policy = verify_policy(script, frame, until, report)
    (report / 'preview-policy.json').write_text(json.dumps(policy, indent=2))
    (report / 'previews.json').write_text(json.dumps({'status':'passed','native_bridge':True,'formats':['code','markdown','svg','html'],'local_css_import':True,'local_classic_script':True,'opaque_parent':True,'no_native_bridge':True,'explicit_refresh':True,'frame_reset':True,'connect_src_violation':True},indent=2))
