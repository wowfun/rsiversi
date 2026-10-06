import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class NativeGateTests(unittest.TestCase):
    def test_actual_native_step_propagates_either_cargo_failure(self):
        workflow = Path(__file__).resolve().parents[3] / '.github/workflows/ci.yml'
        step = workflow.read_text().split('      - name: Prove native Browser confinement and abrupt-owner retirement\n', 1)[1]
        run = step.split('        run: |\n', 1)[1].split('      - name:', 1)[0]
        script = '\n'.join(line[10:] for line in run.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            programs = {
                'cargo': '#!/bin/bash\nif [[ "$*" == *--list* ]]; then\n echo "native_preview_check_and_retirement: test"\n echo "native_abrupt_owner_death_reaps_the_entire_cgroup: test"\nelif [[ "$*" == *"$FAILING_TEST"* ]]; then exit 23; fi\n',
                'sudo': '#!/bin/bash\nexit 0\n',
                'sysctl': '#!/bin/bash\necho 0\n',
            }
            for name, content in programs.items():
                path = root / name
                path.write_text(content)
                path.chmod(0o700)
            for name in ('native_preview_check_and_retirement', 'native_abrupt_owner_death_reaps_the_entire_cgroup'):
                with self.subTest(test=name):
                    env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}', RUNNER_TEMP=directory, FAILING_TEST=name)
                    result = subprocess.run(['bash', '-e', '-c', script], env=env, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 23, result.stdout.decode() + result.stderr.decode())
