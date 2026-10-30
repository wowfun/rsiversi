"""Readiness may reacquire stale handles; native keyboard gestures are never replayed."""
from pathlib import Path

FOCUS_INPUT = (Path(__file__).with_name('focus-input.js')).read_text()
COMPOSER_FOCUS = (Path(__file__).with_name('composer-focus.js')).read_text()
ASSET_PRESSURE = (Path(__file__).with_name('asset-pressure.js')).read_text()
BUTTON_TARGET = (Path(__file__).with_name('button-target.js')).read_text()


def fill(script, keys, until, css, value):
    def ready():
        try:
            item = script(FOCUS_INPUT, [css])
            if not item:
                return None
            state = script('return {value:arguments[0].value,events:window.fixtureInputEvents?.length??0}', [item])
            return item, state
        except RuntimeError as error:
            if not error.args or not isinstance(error.args[0], dict):
                raise
            failure = error.args[0].get('value')
            if not isinstance(failure, dict) or failure.get('error') != 'stale element reference':
                raise
            return None
    item, before = until(ready)
    keys(item, '\ue009a\ue000\ue003')
    until(lambda: script(FOCUS_INPUT, [css, 'value']) == '')
    if css == 'textarea[aria-label="Main message"]' and before['value']:
        until(lambda: script('return window.fixtureInputEvents.slice(arguments[0]).some(e=>e.trusted&&e.length===0)', [before['events']]))
    if value:
        item, _ = until(ready)
        keys(item, value)
    until(lambda: script(FOCUS_INPUT, [css, 'value']) == value)


def asset_pressure(script, until):
    return until(lambda: script(ASSET_PRESSURE))


def file_has_bytes(path, expected):
    try:
        with path.open('rb') as source:
            return source.read(len(expected) + 1) == expected
    except FileNotFoundError:
        return False


def configure_live_limits(script, fill_input, limits):
    script('return document.querySelector(\'[aria-label="Model settings 1"]\').click()')
    labels = dict(context_window_tokens="Context window", default_output_reserve_tokens="Default output tokens", max_output_reserve_tokens="Max output tokens")
    for field, value in limits.items():
        fill_input(f'[aria-label="{labels[field]} 1"]', str(value))
