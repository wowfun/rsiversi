import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import coding


class HeadlessEvidence(unittest.TestCase):
    @unittest.skipUnless(
        os.name == "posix", "oracle supervisor uses Unix process groups"
    )
    def test_raw_process_output_preserves_invalid_utf8(self):
        result = coding.run_oracle_process(
            [
                sys.executable,
                "-c",
                "import os; os.write(1,b'\\xffok'); os.write(2,b'\\xfebad')",
            ],
            dict(os.environ),
            5,
            decode_output=False,
        )
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["stdout"], b"\xffok")
        self.assertEqual(result["stderr"], b"\xfebad")

    def test_integrity_failure_persists_each_task_and_continues_batch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "unused-driver"
            binary.write_bytes(b"stub")
            args = SimpleNamespace(
                binary=binary,
                key_file=root / "unused",
                output=root / "results",
                task=None,
                model="fixture",
            )
            with (
                patch.object(coding, "read_key", return_value="fixture-secret"),
                patch.object(Path, "is_relative_to", return_value=False),
                patch.object(coding, "TASKS", coding.TASKS[:2]),
                patch.object(
                    coding,
                    "oracle",
                    side_effect=ValueError("fixture-secret integrity failure"),
                ) as oracle,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                coding.live(args)
            self.assertEqual(oracle.call_count, 2)
            text = (args.output / "results.json").read_text()
            self.assertNotIn("fixture-secret", text)
            reports = json.loads(text)
            self.assertEqual(len(reports), 2)
            for report in reports:
                self.assertEqual(report["classification"], "infrastructure_failure")
                self.assertEqual(
                    json.loads(
                        (args.output / report["task"] / "report.json").read_text()
                    ),
                    report,
                )

    def test_live_artifact_writes_original_agent_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "unused-driver"
            binary.write_bytes(b"stub")
            task = dict(coding.TASKS[0], prompts=["fixture"])
            args = SimpleNamespace(
                binary=binary,
                key_file=root / "unused",
                output=root / "results",
                task=None,
                model="fixture",
            )
            execution = dict(
                exit_code=0,
                stdout=bytearray(b"\xffexact\n"),
                stderr=bytearray(b"\xfewarning"),
                timed_out=False,
            )
            with (
                patch.object(coding, "read_key", return_value="fixture-secret"),
                patch.object(Path, "is_relative_to", return_value=False),
                patch.object(coding, "TASKS", [task]),
                patch.object(
                    coding,
                    "oracle",
                    side_effect=[dict(passed=False), dict(passed=True)],
                ),
                patch.object(coding, "agent_command", return_value=["unused"]),
                patch.object(
                    coding,
                    "run_oracle_process",
                    side_effect=[dict(exit_code=0), execution],
                ) as process,
                patch.object(coding, "durable_trace", return_value=dict(facts=[])),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                coding.live(args)
            self.assertFalse(process.call_args.kwargs["decode_output"])
            destination = args.output / task["id"]
            self.assertEqual(
                (destination / "stage-1.jsonl").read_bytes(), b"\xffexact\n"
            )
            self.assertEqual(
                (destination / "stage-1.stderr").read_bytes(), b"\xfewarning"
            )


if __name__ == "__main__":
    unittest.main()
