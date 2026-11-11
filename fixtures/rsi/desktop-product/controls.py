"""Readiness may reacquire stale handles; native keyboard gestures are never replayed."""
from pathlib import Path

FOCUS_INPUT = (Path(__file__).with_name('focus-input.js')).read_text()
COMPOSER_FOCUS = (Path(__file__).with_name('composer-focus.js')).read_text()
ASSET_PRESSURE = (Path(__file__).with_name('asset-pressure.js')).read_text()
BUTTON_TARGET = (Path(__file__).with_name('button-target.js')).read_text()
SAVED_INPUT = (Path(__file__).with_name('saved-input.js')).read_text()


def stale_target(error):
    reply = error.args[0] if error.args else None
    failure = reply.get('value') if isinstance(reply, dict) else None
    return isinstance(failure, dict) and failure.get('error') == 'stale element reference'


def click(resolve, dispatch, until):
    def attempt():
        item = resolve()
        if not item:
            return False
        try:
            dispatch(item)
        except RuntimeError as error:
            if not stale_target(error):
                raise
            return False
        return True
    until(attempt)


def fill(script, keys, until, css, value):
    def ready():
        try:
            item = script(FOCUS_INPUT, [css])
            if not item:
                return None
            state = script('return {value:arguments[0].value,events:window.fixtureInputEvents?.length??0}', [item])
            return item, state
        except RuntimeError as error:
            if not stale_target(error):
                raise
            return None
    def send(text):
        def attempt():
            current = ready()
            if not current:
                return None
            try:
                keys(current[0], text)
            except RuntimeError as error:
                if not stale_target(error):
                    raise
                return None
            return current
        return until(attempt)
    _, before = send('\ue009a\ue000\ue003')
    until(lambda: script(FOCUS_INPUT, [css, 'value']) == '')
    if css == 'textarea[aria-label="Main message"]' and before['value']:
        until(lambda: script('return window.fixtureInputEvents.slice(arguments[0]).some(e=>e.trusted&&e.length===0)', [before['events']]))
    if value:
        send(value)
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
