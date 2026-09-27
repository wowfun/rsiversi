"""Build the independent embedder from the current paired capture and verify it."""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('report', type=Path)
args = parser.parse_args()
root = Path(__file__).resolve().parents[3]
cache = root / 'target/rsi-app'
report = args.report.resolve()
report.mkdir(parents=True, exist_ok=False)
spec = importlib.util.spec_from_file_location('distribution', root / 'apps/devtools/distribution.py')
distribution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(distribution)
with (cache / 'build.lock').open('a+b') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    bundle = (cache / 'current').resolve(strict=True)
    pin = (bundle / '.rsi-generation.lock').open('rb')
    fcntl.flock(pin, fcntl.LOCK_SH)
    manifest_path = cache / 'build/build-family.json'
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    family = hashlib.sha256(manifest_bytes).hexdigest()
    assert json.loads((bundle / 'receipt.json').read_text())['family_sha256'] == family, 'Build a successful paired publication first'
    source = cache / 'build/source'
    distribution.verify_capture(source, manifest['files'])
    env = {name: os.environ[name] for name in ('PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'CARGO_BUILD_JOBS') if name in os.environ}
    for name, suffix in [('CARGO_HOME', '.cargo'), ('RUSTUP_HOME', '.rustup')]:
        env.setdefault(name, str(Path.home() / suffix))
    env.update(HOME=str(cache / 'build-home'), XDG_CACHE_HOME=str(cache / 'tool-cache'),
               RSI_BUILD_FAMILY_MANIFEST=str(manifest_path), RSI_BUILD_FAMILY_SHA256=family,
               CARGO_TARGET_DIR=str(cache / 'build/target'), **manifest['flags'])
    command = ['cargo', 'build', '--locked', '--target', manifest['target'], '-p', 'rsi-cli', '--example', 'workbench-addon']
    if manifest['profile'] == 'release': command.append('--release')
    with (report / 'build.log').open('w') as log:
        subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    distribution.verify_capture(source, manifest['files'])
    binary = report / 'paired-workbench-addon'
    shutil.copy2(cache / 'build/target' / manifest['target'] / manifest['profile'] / 'examples/workbench-addon', binary)
    (report / 'fixture-receipt.json').write_text(json.dumps({'family': family, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'bundle': str(bundle), 'command': command}, indent=2))
try:
    env = {name: os.environ[name] for name in ('PATH', 'LD_LIBRARY_PATH', 'PLAYWRIGHT_BROWSERS_PATH', 'HOME') if name in os.environ}
    env.update(RSI_WORKBENCH_BINARY=str(binary), RSI_WEB_ASSETS=str(bundle / 'assets'), RSI_WORKBENCH_REPORT=str(report / 'browser'))
    subprocess.run(['node', str(root / 'fixtures/rsi/addon-workbench/verify.mjs')], cwd=root, env=env, check=True)
finally:
    pin.close()
