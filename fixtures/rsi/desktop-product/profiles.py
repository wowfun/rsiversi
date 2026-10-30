"""Reviewed Host source changes by the managed Desktop Device after a Local grant."""
import json
import re
import subprocess


def verify(script, button, click, until, screenshot, source, report, probe, env):
    def local(request):
        result = subprocess.run([str(probe.resolve())], input=json.dumps(request), env=env,
                                text=True, capture_output=True, timeout=30, check=True)
        value = json.loads(result.stdout)
        assert 'Ok' in value, value
        return value['Ok']

    original = source.read_bytes()
    button('Settings')
    button('Plugins')
    button('Browse Host Profiles')
    button('Open Profile editable')
    until(lambda: script(r'return [...document.querySelectorAll(".profile-leaves option")].some(e=>e.value==="rsi-inspector-api")'))
    script(r'const e=document.querySelector(".profile-leaves select");e.value="rsi-inspector-api";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    until(lambda: script(r'return document.querySelector(".profile-leaves h4")?.textContent.includes("Profile editable")&&[...document.querySelectorAll(".profile-leaves button")].some(e=>e.textContent==="Preview disable")'))
    script(r'document.querySelector(".profile-leaves").scrollIntoView({block:"start"});return true')
    caller = script(r'return [...document.querySelectorAll(".profile-leaves > p")].find(e=>e.textContent.startsWith("Caller:"))?.textContent')
    device = re.match(r'Caller: device ([0-9a-f]{32})(?:\s|$)', caller or '')
    assert device, caller
    assert script(r'return ![...document.querySelectorAll(".profile-leaves summary")].some(e=>e.textContent==="Grant an exact change")')
    assert script(r'return [...document.querySelectorAll(".profile-leaves button")].find(e=>e.textContent==="Preview disable").disabled')
    target = next(leaf['target'] for leaf in local({'operation':'catalog','query':{'profile':'editable','after':None}})['leaves'] if leaf['target']['leaf'] == 'rsi-inspector-api')
    scope = {'principal':{'kind':'device','id':device[1]},'scope':{'kind':'profile','target':target,'operation':'disable'}}
    local({'operation':'set_grant','request':{'expected':local({'operation':'grants'})['revision'],'scope':scope,'granted':True}})
    button('Refresh source choices')
    button('Preview disable')
    until(lambda: script(r'return !!document.querySelector("[aria-label=\"Prepared rsi-inspector-api\"]")'))
    assert source.read_bytes() == original
    screenshot('profile-review.png')
    button('Save reviewed change')
    until(lambda: script(r'const e=document.querySelector("[aria-label=\"Profile source receipt\"]");return e?.textContent.includes("saved")&&e.textContent.includes("not selected")'))
    saved = source.read_bytes()
    assert saved != original and b'kind = "patch"' in saved
    screenshot('profile-saved.png')
    button('Query original receipt')
    until(lambda: script(r'return ![...document.querySelectorAll("button")].find(e=>e.textContent==="Query original receipt")?.disabled'))
    assert source.read_bytes() == saved
    ticket = script(r'return document.querySelector("[aria-label=\"Profile source receipt\"] code").textContent')
    button('Close settings')
    button('Settings')
    button('Plugins')
    button('Browse Host Profiles')
    until(lambda: script(r'return [...document.querySelectorAll(".profile-leaves button")].some(e=>e.textContent==="Open Profile editable"&&!e.disabled)'))
    click(script(r'return [...document.querySelectorAll(".profile-leaves summary")].find(e=>e.textContent.startsWith("Recover a source receipt"))'))
    until(lambda: script(r'return [...document.querySelectorAll(".profile-leaves details")].some(e=>e.open&&e.querySelector("summary")?.textContent.startsWith("Recover a source receipt"))'))
    script(r'const e=[...document.querySelectorAll(".profile-leaves select")].find(e=>[...e.options].some(o=>o.value===arguments[0]));e.value=arguments[0];e.dispatchEvent(new Event("change",{bubbles:true}));return true', [ticket])
    button('Read selected receipt')
    until(lambda: script(r'return document.querySelector("[aria-label=\"Profile source receipt\"]")?.textContent.includes("saved")'))
    assert source.read_bytes() == saved
    button('Close settings')
    (report / 'profiles.json').write_text(json.dumps({'status':'passed','device_requires_local_grant':True,'prepared_before_write':True,'saved_once':True,'application':'not_selected','original_receipt_after_reopen':True},indent=2))
