"""Verify the generated linked addon against its unchanged pinned SDK lockfile."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--report', type=Path, required=True)
args = parser.parse_args()
report = args.report.resolve()
report.mkdir(parents=True, exist_ok=False)
repository = Path(__file__).resolve().parents[3]

def run(name, command, cwd):
    with (report / (name + '.log')).open('w') as log:
        subprocess.run(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=900)

with tempfile.TemporaryDirectory(prefix='rsi-linked-template-') as directory:
    project = Path(directory) / 'generated'
    run('generate', ['cargo', 'xtask', 'addon', 'new', 'generated-proof', '--directory', str(project), '--kind', 'linked'], repository)
    lock = project / 'Cargo.lock'
    digest = hashlib.sha256(lock.read_bytes()).hexdigest()
    for name in ['test', 'run']:
        run(name, ['cargo', name, '--locked', '--target-dir', str(repository / 'target/linked-template')], project)
        assert hashlib.sha256(lock.read_bytes()).hexdigest() == digest, 'Generated lockfile changed'
    (report / 'result.json').write_text(json.dumps({'ok': True, 'generated': True, 'lockfile_unchanged': True, 'lock_sha256': digest}, indent=2) + '\n')
