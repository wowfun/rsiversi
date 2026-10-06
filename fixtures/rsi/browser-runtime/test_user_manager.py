import importlib.util
import os
from pathlib import Path
import pwd
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("user_manager", Path(__file__).with_name("user_manager.py"))
manager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manager)


class ManagerLifecycle(unittest.TestCase):
    def exercise(self, active, linger, fail_start=False):
        host = {"active": active, "linger": linger}
        def run(args, **kwargs):
            if args[:2] == ["systemctl", "is-active"]:
                return subprocess.CompletedProcess(args, 0 if host["active"] else 3)
            action = args[2]
            if action == "start" and fail_start:
                raise subprocess.CalledProcessError(1, args)
            if action in ("start", "stop"):
                host["active"] = action == "start"
            elif action in ("enable-linger", "disable-linger"):
                host["linger"] = action == "enable-linger"
            else:
                raise AssertionError(args)
            return subprocess.CompletedProcess(args, 0)
        with tempfile.TemporaryDirectory() as directory, patch.object(manager.subprocess, "run", side_effect=run):
            root = Path(directory)
            if linger:
                (root / pwd.getpwuid(os.getuid()).pw_name).touch()
            state = root / "state.json"
            if fail_start:
                with self.assertRaises(subprocess.CalledProcessError):
                    manager.prepare(state, root)
            else:
                manager.prepare(state, root)
                self.assertEqual(host, {"active": True, "linger": True})
            manager.restore(state)
            self.assertEqual(host, {"active": active, "linger": linger})
            self.assertFalse(state.exists())
            manager.restore(state)

    def test_owned_and_preexisting_resources(self):
        for active in (False, True):
            for linger in (False, True):
                with self.subTest(active=active, linger=linger):
                    self.exercise(active, linger)

    def test_partial_setup_failure_restores_owned_linger(self):
        self.exercise(False, False, fail_start=True)

    def test_invalid_owner_does_not_change_host_state(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(manager.subprocess, "run") as run:
            path = Path(directory) / "state.json"
            path.write_text('{"uid":-1,"user":"wrong","active":false,"linger":false}')
            with self.assertRaises(RuntimeError):
                manager.restore(path)
            run.assert_not_called()
            self.assertTrue(path.exists())


if __name__ == "__main__":
    unittest.main()
