"""Bounded, credential-redacted failure evidence for the desktop fixture."""
import json
import sys


def run_cleanup(steps, failure=None):
    errors = []
    for phase, action in steps:
        try:
            action()
        except Exception as error:
            if failure is not None:
                failure.add_note(f'Cleanup {phase}: {type(error).__name__}: {error}')
            else:
                error.add_note(f'Cleanup phase: {phase}')
                errors.append(error)
    if errors:
        raise ExceptionGroup('Desktop cleanup failed', errors)


def scrub_text(text, secret):
    variants = {secret, json.dumps(secret, ensure_ascii=True)[1:-1], json.dumps(secret, ensure_ascii=False)[1:-1]}
    for value in sorted(variants, key=len, reverse=True):
        text = text.replace(value, '[REDACTED]')
    return text


def scrub_json(value, secret):
    if isinstance(value, str):
        return value.replace(secret, '[REDACTED]')
    if isinstance(value, list):
        return [scrub_json(item, secret) for item in value]
    if isinstance(value, dict):
        return {scrub_json(key, secret): scrub_json(item, secret) for key, item in value.items()}
    return value


def redact_evidence(directory, secret, failure=None):
    """Scrub the literal key from text evidence without replacing an active failure."""
    if not secret:
        return
    leaked = False
    symlink = False
    for path in directory.rglob('*'):
        if path.is_symlink():
            symlink = True
            continue
        if path.is_file() and path.suffix in ('.json', '.txt', '.log', '.html'):
            source = path.read_text(errors='replace')
            try:
                value = json.loads(source)
                clean = scrub_json(value, secret)
                replacement = json.dumps(clean, ensure_ascii=False, indent=2) if value != clean else source
            except ValueError:
                replacement = scrub_text(source, secret)
            if replacement != source:
                path.write_text(replacement)
                leaked = True
    if leaked or symlink:
        message = '; '.join(text for flag, text in [(leaked, 'Live evidence contained a credential and was redacted'), (symlink, 'Evidence contains a symlink; its target was not read or modified')] if flag)
        if failure is None:
            raise RuntimeError(message)
        failure.add_note(message)


def record_failure(directory, phase, error, daemon, webdriver, secret=None):
    def redact(text):
        return scrub_text(text, secret) if secret else text

    failure = {
        'phase': phase,
        'error': str(error).replace(secret, '[REDACTED]') if secret else str(error),
        'daemonExitCode': daemon.poll() if daemon else None,
        'webdriverExitCode': webdriver.poll() if webdriver else None,
    }
    text = json.dumps(failure, ensure_ascii=False, indent=2)
    if (directory / 'failure.json').is_symlink():
        raise ValueError('Refusing a symlinked failure report')
    (directory / 'failure.json').write_text(text)
    print(text, file=sys.stderr)
    for name in ('daemon.log', 'webdriver.log'):
        path = directory / name
        if path.exists() and not path.is_symlink():
            with path.open('rb') as evidence:
                evidence.seek(max(0, path.stat().st_size - 65536))
                tail = evidence.read(65536).decode('utf-8', errors='replace')
            print(f'{name} (bounded tail):\n{redact(tail)}', file=sys.stderr)
