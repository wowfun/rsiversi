import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import campaign


class Campaign(unittest.TestCase):
    def test_provenance_includes_untracked_campaign_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            root = temporary / "repo"
            fixture = root / "fixture"
            (fixture / "corpus/apply_patch").mkdir(parents=True)
            (fixture / "corpus/apply_patch/seed").write_bytes(b"seed")
            (fixture / "Cargo.lock").write_bytes(b"locked")
            environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
            environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, env=environment, text=True).strip()
            git("init", "--quiet")
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture")
            native_run = subprocess.run
            def capture(*args):
                if args[0] == "git":
                    return git(*args[1:])
                return "cargo-fuzz 0.13.2" if args[1] == "fuzz" else "fixture-rust"
            def run(command, **kwargs):
                return subprocess.CompletedProcess(command, 0) if command[0] == "cargo" else native_run(command, **kwargs)
            with patch.object(campaign, "capture", side_effect=capture), patch.object(campaign.subprocess, "run", side_effect=run), patch.object(campaign.platform, "platform", return_value="fixture"):
                for index, name in enumerate([None, "campaign.py", "harness.rs", "corpus/apply_patch/local-seed"]):
                    with self.subTest(input=name):
                        path = fixture / name if name else None
                        if path:
                            path.write_bytes(b"local input")
                        output = temporary / f"output-{index}"
                        self.assertEqual(campaign.run("apply_patch", output, fixture), 0)
                        metadata = json.loads((output / "metadata.json").read_text())
                        self.assertEqual(metadata["dirty"], path is not None)
                        if path:
                            path.unlink()

    def exercise(self, exit_code=0, artifact_bytes=None, change_lock=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture"
            seeds = fixture / "corpus/apply_patch"
            seeds.mkdir(parents=True)
            (seeds / "seed").write_bytes(b"seed")
            lock = fixture / "Cargo.lock"
            lock.write_bytes(b"locked")
            output = root / "output"
            generated = []

            def probe(command, **kwargs):
                first = Path(command[command.index("apply_patch") + 1])
                second = Path(command[command.index("apply_patch") + 2])
                self.assertNotEqual(first, second)
                self.assertEqual(second, seeds)
                (first / "generated").write_bytes(b"generated")
                generated.append(first)
                self.assertIn("-max_total_time=60", command)
                self.assertIn("-timeout=60", command)
                self.assertIn("-max_len=2000001", command)
                if artifact_bytes is not None:
                    (output / "failure.bin").write_bytes(artifact_bytes)
                if change_lock:
                    lock.write_bytes(b"changed")
                return subprocess.CompletedProcess(command, exit_code)

            with patch.object(campaign, "capture", side_effect=lambda *args: "cargo-fuzz 0.13.2" if args[1] == "fuzz" else "fixture"), patch.object(campaign.platform, "platform", return_value="fixture"), patch.object(campaign.subprocess, "run", side_effect=probe):
                status = campaign.run("apply_patch", output, fixture)
            self.assertEqual((seeds / "seed").read_bytes(), b"seed")
            self.assertTrue(all(not path.exists() for path in generated))
            metadata = json.loads((output / "metadata.json").read_text())
            return status, (output / "failure.bin").exists(), metadata

    def test_success_removes_slow_artifact_and_generated_corpus(self):
        status, artifact, report = self.exercise(artifact_bytes=b"slow")
        self.assertEqual(status, 0)
        self.assertFalse(artifact)
        self.assertEqual(report["results"]["exit_code"], 0)

    def test_failure_retains_one_bounded_input_and_actual_exit(self):
        status, artifact, report = self.exercise(exit_code=77, artifact_bytes=b"failure")
        self.assertEqual(status, 77)
        self.assertTrue(artifact)
        self.assertEqual(report["results"]["failure_input"], "failure.bin")

    def test_lock_drift_and_oversized_artifacts_fail(self):
        self.assertNotEqual(self.exercise(change_lock=True)[0], 0)
        status, artifact, report = self.exercise(exit_code=77, artifact_bytes=b"x" * 2_000_002)
        self.assertNotEqual(status, 0)
        self.assertFalse(artifact)
        self.assertIn("exceeds", report["results"]["error"])

    def test_missing_seeds_retain_failed_provenance_without_running(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.lock").write_bytes(b"locked")
            with patch.object(campaign.subprocess, "run") as probe:
                self.assertNotEqual(campaign.run("apply_patch", root / "out", root), 0)
            probe.assert_not_called()
            self.assertTrue((root / "out/metadata.json").is_file())
