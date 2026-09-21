"""Reviewed Host source changes through the actual Local GUI and native bridge."""
import json


def verify(script, button, click, until, screenshot, source, report):
    original = source.read_bytes()
    button('Settings')
    button('Plugins')
    button('Browse Host Profiles')
    button('Open Profile editable')
    until(lambda: script(r'return [...document.querySelectorAll(".profile-leaves option")].some(e=>e.value==="rsi-inspector-api")'))
    script(r'const e=document.querySelector(".profile-leaves select");e.value="rsi-inspector-api";e.dispatchEvent(new Event("change",{bubbles:true}));return true')
    until(lambda: script(r'return document.querySelector(".profile-leaves h4")?.textContent.includes("Profile editable")&&[...document.querySelectorAll(".profile-leaves summary")].some(e=>e.textContent==="Grant an exact change")'))
    script(r'document.querySelector(".profile-leaves").scrollIntoView({block:"start"});return true')
    grant = script(r'return [...document.querySelectorAll(".profile-leaves summary")].find(e=>e.textContent==="Grant an exact change")')
    click(grant)
    button('Read grant revision')
    button('Grant this exact change')
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
    click(script(r'return [...document.querySelectorAll(".profile-leaves summary")].find(e=>e.textContent.startsWith("Recover a source receipt"))'))
    script(r'const e=[...document.querySelectorAll(".profile-leaves select")].find(e=>[...e.options].some(o=>o.value===arguments[0]));e.value=arguments[0];e.dispatchEvent(new Event("change",{bubbles:true}));return true', [ticket])
    button('Read selected receipt')
    until(lambda: script(r'return document.querySelector("[aria-label=\"Profile source receipt\"]")?.textContent.includes("saved")'))
    assert source.read_bytes() == saved
    button('Close settings')
    (report / 'profiles.json').write_text(json.dumps({'status':'passed','local_grant':True,'prepared_before_write':True,'saved_once':True,'application':'not_selected','original_receipt_after_reopen':True},indent=2))
