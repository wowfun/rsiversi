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
                'cargo': '#!/bin/bash\nif [[ "$*" == *--list* ]]; then\n echo "native_preview_check_and_retirement: test"\n echo "native_abrupt_owner_death_reaps_the_entire_cgroup: test"\n echo "browser::session_browser_real_http_ws_nodes_images_and_presentation_detach: test"\nelif [[ -n "$FAILING_TEST" && "$*" == *"$FAILING_TEST"* ]]; then exit 23; fi\n',
                'sudo': '#!/bin/bash\nexit 97\n',
                'sysctl': '#!/bin/bash\nexit 98\n',
            }
            helper = root / '.github/scripts/sandbox-policy.py'
            helper.parent.mkdir(parents=True)
            helper.write_text('import os, sys\nfrom pathlib import Path\nwith Path(os.environ["POLICY_CALLS"]).open("a") as out: out.write(sys.argv[1]+"\\n")\n', encoding='utf-8')
            wrapper = workflow.parent.parent / 'scripts/sandbox-policy.sh'
            (helper.parent / 'sandbox-policy.sh').write_bytes(wrapper.read_bytes())
            self.assertIn('source "$GITHUB_WORKSPACE/.github/scripts/sandbox-policy.sh" full', script)
            calls = root / 'policy-calls'
            for name, content in programs.items():
                path = root / name
                path.write_text(content)
                path.chmod(0o700)
            for name in ('native_preview_check_and_retirement', 'native_abrupt_owner_death_reaps_the_entire_cgroup', ''):
                with self.subTest(test=name):
                    calls.write_text('')
                    env = dict(os.environ, PATH=f'{root}{os.pathsep}{os.environ["PATH"]}', RUNNER_TEMP=directory, FAILING_TEST=name, GITHUB_WORKSPACE=directory, POLICY_CALLS=str(calls))
                    result = subprocess.run(['bash', '-e', '-c', script], env=env, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 23 if name else 0, result.stdout.decode(errors="replace") + result.stderr.decode(errors="replace"))
                    self.assertEqual(calls.read_text().splitlines(), ['prepare', 'restore'])
