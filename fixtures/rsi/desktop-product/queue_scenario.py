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
    fill(composer, 'Desktop draft remains intact')
    button('Edit')
    fill('textarea[aria-label="Queued text 1"]', 'Desktop queue edited')
    screenshot('queue-editor.png')
    button('Save replacement')
    until(lambda: script('return !document.querySelector(".queue-editor")'))
    until(lambda: script('return [...document.querySelectorAll(".message.user")].filter(e=>e.textContent.includes("Desktop queue edited")).length === 1'))
    assert script('return document.querySelector(".queue-row").dataset.slot') == slot
    assert script('return [...document.querySelectorAll(".message.user")].filter(e=>e.textContent.includes("Desktop queue original")).length') == 0
    button('Steer now')
    until(lambda: script('return document.querySelector(".queue-row").textContent.includes("Next step")'))
    assert script('return document.querySelector(".queue-row").dataset.slot') == slot
    button('Withdraw')
    until(lambda: script('return document.querySelectorAll(".queue-row").length === 1'))
    screenshot('queue-before-stop.png')
    button('Stop')
    until(lambda: any(item.get('prompt') == 'Desktop queue survivor' for item in requests))
    until(lambda: script('return document.querySelector(".pane-status").textContent === "Completed"'))
    assert script('return document.querySelector(arguments[0]).value', [composer]) == 'Desktop draft remains intact'
    assert not any(item.get('prompt') in ('Desktop queue original', 'Desktop queue edited') for item in requests)
    screenshot('queue-after-stop.png')
    (report / 'queue.json').write_text(json.dumps({'replacementKeepsSlot': True, 'oneUserBlock': True, 'exactTurnConversion': True, 'pendingOnlyWithdrawal': True, 'stopPreservesQueueAndDraft': True}, indent=2))
    fill(composer, '')
