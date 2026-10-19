"""Opt-in fixture credentials: one explicit UTF-8 file, at most 64 KiB, no env fallback."""
import re
import os


def read_deepseek_key(path):
    with path.open('rb') as source:
        value = source.read(65537)
    if len(value) > 65536:
        raise ValueError('live environment exceeds its bound')
    match = re.search(r'^[ \t]*(?:export[ \t]+)?DEEPSEEK_API_KEY[ \t]*=[ \t]*(.*?)[ \t]*\r?$', value.decode('utf-8'), re.M)
    if not match:
        raise ValueError('authorized DeepSeek key is absent')
    key = re.sub(r'''^(["'])(.*)\1$''', r'\2', match[1].strip())
    if not key.strip():
        raise ValueError('authorized DeepSeek key is empty')
    return key


def live_limits(environment=None):
    environment = os.environ if environment is None else environment
    limits = {}
    for field in ('context_window_tokens', 'default_output_reserve_tokens', 'max_output_reserve_tokens'):
        name = 'DEEPSEEK_' + field.upper()
        raw = environment.get(name)
        if not isinstance(raw, str) or not re.fullmatch(r'[1-9][0-9]{0,9}', raw) or int(raw) > 0xffffffff:
            raise ValueError(f'{name} must be an explicit positive integer fitting u32')
        limits[field] = int(raw)
    if limits['max_output_reserve_tokens'] < limits['default_output_reserve_tokens']:
        raise ValueError('maximum output reserve must accommodate the default output reserve')
    return limits
