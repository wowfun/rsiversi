"""Pending queue operations through the actual native WebKit document and Rust bridge."""
import json


def verify(script, button, fill, until, screenshot, report, requests):
    composer = 'textarea[aria-label="Main message"]'

    def send(text):
        fill(composer, text)
        button('Send')
        until(lambda: script('return document.querySelector(arguments[0]).value === ""', [composer]))

    send('Desktop queue hold active')
    until(lambda: script('return document.querySelector(".transcript").textContent.includes("Queue fixture is waiting.")'))
    send('Desktop queue original')
    send('Desktop queue survivor')
    until(lambda: script('return document.querySelectorAll(".queue-row").length === 2'))
    slot = script('return document.querySelector(".queue-row").dataset.slot')
    scope = f'.queue-row[data-slot={json.dumps(slot)}]'
    original = script('return document.querySelector(arguments[0]).querySelector(".queue-summary").textContent', [scope])
    fill(composer, 'Desktop draft remains intact')
    button('Edit', scope)
    fill('textarea[aria-label="Queued text 1"]', 'Desktop queue edited')
    screenshot('queue-editor.png')
    button('Save replacement')
    until(lambda: script('return !document.querySelector(".queue-editor")'))
    until(lambda: script('const row=document.querySelector(arguments[0]);return row && row.querySelector(".queue-summary").textContent!==arguments[1]', [scope, original]))
    button('Edit', scope)
    until(lambda: script("return document.querySelector('textarea[aria-label=\"Queued text 1\"]')?.value === 'Desktop queue edited'"))
    button('Close')
    until(lambda: script('return !document.querySelector(".queue-editor")'))
    assert script('return [...document.querySelectorAll(".message.user")].filter(e=>e.textContent.includes("Desktop queue edited")).length') == 0
    assert script('return document.querySelector(".queue-row").dataset.slot') == slot
    assert script('return [...document.querySelectorAll(".message.user")].filter(e=>e.textContent.includes("Desktop queue original")).length') == 0
    assert script('return ![...document.querySelector(".queue-row").querySelectorAll("button")].some(e=>e.textContent==="Steer now")')
    button('Withdraw', scope)
    until(lambda: script('return document.querySelectorAll(".queue-row").length === 1'))
    screenshot('queue-before-stop.png')
    button('Stop')
    until(lambda: any(item.get('prompt') == 'Desktop queue survivor' for item in requests))
    until(lambda: script('return document.querySelector(".pane-status").textContent === "Completed"'))
    assert script('return document.querySelector(arguments[0]).value', [composer]) == 'Desktop draft remains intact'
    assert not any(item.get('prompt') in ('Desktop queue original', 'Desktop queue edited') for item in requests)
    assert script('return [...document.querySelectorAll(".message.user")].filter(e=>e.textContent.includes("Desktop queue survivor")).length') == 1
    screenshot('queue-after-stop.png')
    (report / 'queue.json').write_text(json.dumps({'replacementKeepsSlot': True, 'pendingOnlyInQueue': True, 'replacementReadback': True, 'explicitAccessPreventsSteering': True, 'pendingOnlyWithdrawal': True, 'stopPreservesQueueAndDraft': True}, indent=2))
    fill(composer, '')
