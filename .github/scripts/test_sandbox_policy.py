import importlib.util
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("sandbox_policy", Path(__file__).with_name("sandbox-policy.py"))
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class PolicyLifecycle(unittest.TestCase):
    def setUp(self):
        owner = patch.object(policy, "owner", return_value=123)
        owner.start()
        self.addCleanup(owner.stop)

    def exercise(self, initial, fail_prepare=False, fail_restore=False):
        host = dict(initial)
        restoring = False
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / "state.json"
            for key in host:
                path = root / key.replace(".", "/")
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()

            def command(*args):
                if args[0] == "sysctl":
                    return subprocess.CompletedProcess(args, 0, host[args[2]])
                self.assertTrue(state.is_file(), "ownership record must precede every mutation")
                if os.name == "posix":
                    self.assertEqual(state.stat().st_mode & 0o777, 0o600)
                key, value = args[-1].split("=")
                if ((fail_prepare and not restoring and key.endswith("unprivileged_userns"))
                        or (fail_restore and restoring and key.endswith("userns_clone"))):
                    raise subprocess.CalledProcessError(1, args)
                host[key] = value

            with patch.object(policy, "command", side_effect=command):
                if fail_prepare:
                    with self.assertRaises(subprocess.CalledProcessError):
                        policy.prepare(state, "full", root)
                else:
                    policy.prepare(state, "full", root)
                    self.assertEqual(host, {key: policy.SETTINGS[key] for key in initial})
                restoring = True
                if fail_restore:
                    with self.assertRaises(RuntimeError):
                        policy.restore(state)
                    self.assertTrue(state.exists())
                    self.assertEqual(host["kernel.apparmor_restrict_unprivileged_userns"], initial["kernel.apparmor_restrict_unprivileged_userns"])
                    with self.assertRaisesRegex(RuntimeError, "not restored by an earlier step"):
                        policy.prepare(state, "full", root)
                    fail_restore = False
                policy.restore(state)
                self.assertEqual(host, initial)
                self.assertFalse(state.exists())
                policy.restore(state)

    def test_preserves_supported_existing_values_and_missing_full_keys(self):
        keys = list(policy.SETTINGS)
        for first in ("0", "1"):
            for second in ("0", "1"):
                self.exercise(dict(zip(keys, (first, second))))
        self.exercise({keys[1]: "1"})
        self.exercise({})

    def test_partial_setup_failure_is_recoverable(self):
        self.exercise(dict(zip(policy.SETTINGS, ("0", "1"))), fail_prepare=True)

    def test_restoration_attempts_every_key_and_retains_failed_state(self):
        self.exercise(dict(zip(policy.SETTINGS, ("0", "1"))), fail_restore=True)

    def test_invalid_or_oversized_state_never_mutates_host(self):
        for value in ({"uid": 124, "values": {}}, {"uid": True, "values": {}},
                      {"uid": 123, "values": {"other.key": "0"}},
                      {"uid": 123, "values": {next(iter(policy.SETTINGS)): "2"}},
                      "x" * 4097):
            with self.subTest(value=str(value)[:80]), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "state"
                path.write_text(json.dumps(value), encoding="utf-8")
                with patch.object(policy, "command") as command, self.assertRaises(ValueError):
                    policy.restore(path)
                command.assert_not_called()
                self.assertTrue(path.exists())

    def test_existing_record_is_not_replaced(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state"
            path.write_text("existing")
            with patch.object(policy, "command") as command, self.assertRaisesRegex(RuntimeError, "not restored by an earlier step"):
                policy.prepare(path, "full", Path(directory))
            command.assert_not_called()
            self.assertEqual(path.read_text(), "existing")

    def test_apparmor_profile_requires_key_and_does_not_touch_userns(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state"
            with patch.object(policy, "command", side_effect=subprocess.CalledProcessError(1, "sysctl")) as command:
                with self.assertRaises(subprocess.CalledProcessError):
                    policy.prepare(path, "apparmor", Path(directory))
                command.assert_called_once_with("sysctl", "-n", "kernel.apparmor_restrict_unprivileged_userns")
                self.assertFalse(path.exists())

    @unittest.skipUnless(os.name == "posix", "the workflow uses native Bash on Linux")
    def test_actual_shared_trap_preserves_command_and_restore_failures(self):
        script = Path(__file__).with_name("sandbox-policy.sh").resolve()
        setup = f'source "{script}" full'
        for prepare_exit, command_exit, restore_exit, expected in ((0, 0, 0, 0), (0, 37, 0, 37), (0, 37, 23, 37), (0, 0, 23, 1), (19, 0, 23, 19)):
            with self.subTest(prepare=prepare_exit, command=command_exit, restore=restore_exit):
                stub = f'python3() {{ if [[ $2 == restore ]]; then echo restored; return {restore_exit}; else return {prepare_exit}; fi; }}\n'
                result = subprocess.run(["bash", "-e", "-c", stub + setup + f"\nexit {command_exit}"], text=True, capture_output=True,
                                        env={**os.environ, "RUNNER_TEMP": tempfile.gettempdir(), "GITHUB_WORKSPACE": str(script.parents[2])})
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(result.stdout.strip(), "restored")

    def test_workflow_uses_shared_policy_owner_and_guards_exact_ignored_tests(self):
        source = (Path(__file__).parents[1] / "workflows/ci.yml").read_text()
        self.assertNotIn("restore_policy()", source)
        self.assertNotIn('trap restore_policy EXIT', source)
        self.assertIn('source "$GITHUB_WORKSPACE/.github/scripts/sandbox-policy.sh" full', source)
        self.assertIn('source "$GITHUB_WORKSPACE/.github/scripts/sandbox-policy.sh" apparmor', source)
        lines = source.splitlines()
        for index, line in enumerate(lines):
            if "cargo test " not in line or "--ignored" not in line or "--exact" not in line or "--list" in line:
                continue
            # The single execution loop validates its variable against a bounded listing.
            if '"$test"' in line:
                self.assertIn('grep -Fx "$test: test"', lines[index-1])
                continue
            previous = "\n".join(lines[max(0,index-4):index])
            filters = re.findall(r'(?:\S+::)*[a-z][a-z_]+', line)
            names = [name for name in filters if f"{name}: test" in previous]
            self.assertTrue(names and "--list" in previous, f"unguarded exact ignored test: {line.strip()}")
