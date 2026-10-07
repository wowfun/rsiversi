"""Readiness precedes native keyboard input; read-only probes are never replayed."""
from pathlib import Path

FOCUS_INPUT = (Path(__file__).with_name('focus-input.js')).read_text()
ASSET_PRESSURE = (Path(__file__).with_name('asset-pressure.js')).read_text()


def fill(script, keys, until, css, value):
    item = until(lambda: script(FOCUS_INPUT, [css]))
    before = script('return {value:arguments[0].value,events:window.fixtureInputEvents?.length??0}', [item])
    keys(item, '\ue009a\ue000\ue003')
    until(lambda: script('return arguments[0].value', [item]) == '')
    if css == 'textarea[aria-label="Main message"]' and before['value']:
        until(lambda: script('return window.fixtureInputEvents.slice(arguments[0]).some(e=>e.trusted&&e.length===0)', [before['events']]))
    if value:
        keys(item, value)
    until(lambda: script('return arguments[0].value', [item]) == value)


def asset_pressure(script, until):
    return until(lambda: script(ASSET_PRESSURE))
