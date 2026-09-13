import copy
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import coding
import session_api


class EvidenceFailures(unittest.TestCase):
    def test_runtime_path_ignores_an_oversized_ambient_tempdir(self):
        with tempfile.TemporaryDirectory() as directory:
            long = Path(directory) / ("ambient-" + "x" * 150)
            long.mkdir()
            with patch.object(tempfile, "tempdir", str(long)), patch.object(session_api, "configure", wraps=session_api.configure) as configure:
                self.run_fault(trace={"facts": []})
            runtime_root = configure.call_args.args[0]
            self.assertEqual(runtime_root.parent, Path("/tmp"))
            self.assertLess(len(str(runtime_root / "r" / ("x" * 64) / "host.sock").encode()), 108)

    def test_main_keeps_an_exceptional_task_in_results(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "driver"
            binary.write_bytes(b"isolated unused stub")
            key = root / "fixture.env"
            key.write_text("DEEPSEEK_API_KEY=fixture-secret-key\n")
            output = root / "results"
            arguments = ["session_api.py", "--live", "--key-file", str(key), "--binary", str(binary), "--task", "utf8-truncate", "--output", str(output)]
            with patch("sys.argv", arguments), patch.object(session_api, "grade_before_deadline", return_value={"passed": True}), \
                 patch.object(coding, "durable_trace", side_effect=KeyError("missing durable column")), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(session_api.main(), 1)
            reports = json.loads((output / "results.json").read_text())
            self.assertEqual(len(reports), 1)
            self.assertEqual(reports[0]["classification"], "infrastructure_failure")
            self.assertEqual(reports[0], json.loads((output / "utf8-truncate/report.json").read_text()))

    def run_fault(self, trace=None, trace_error=None, model="fixture-model", initial=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "driver"
            binary.write_bytes(b"isolated stub; process is not started")
            destination = root / "report"
            with patch.object(session_api, "grade_before_deadline", return_value=initial or {"passed": True}), \
                 patch.object(coding, "durable_trace", return_value=trace, side_effect=trace_error), \
                 patch.object(coding, "run_oracle_process") as agent:
                report = session_api.run_task(copy.deepcopy(coding.TASKS[0]), binary, destination, model, "fixture-secret-key", None)
            self.assertFalse(agent.called, "invalid pristine input must not start an Agent")
            self.assertEqual(report["classification"], "infrastructure_failure")
            self.assertEqual(json.loads((destination / "report.json").read_text()), report)
            for artifact in destination.iterdir():
                self.assertNotIn("fixture-secret-key", artifact.read_text())
            return report

    def test_null_and_missing_trace_fields_preserve_the_classified_task(self):
        for trace in [{}, {"facts": [{"type": "model_intent", "snapshot": {}, "purpose": None}]},
                      {"facts": [{"type": "model_event", "purpose": "context_compaction", "event": None}]}]:
            with self.subTest(trace=trace):
                report = self.run_fault(trace=trace)
                self.assertIn("initial_oracle", report)
                self.assertIn("evidence_error", report)
        self.run_fault(trace_error=KeyError("missing durable column"))

    def test_credential_refusal_retains_a_redacted_report(self):
        self.run_fault(trace={"facts": []}, model="fixture-secret-key")
        report = self.run_fault(trace={"facts": [{"type": "model_intent", "purpose": None, "snapshot": {"credential": "fixture-secret-key"}}]})
        self.assertEqual(report["actual_model_snapshots"], [{"credential": "[REDACTED]"}])

    def test_pristine_admission_rejection_is_infrastructure_before_execution(self):
        self.run_fault(trace={"facts": []}, initial={"passed": False, "admission_error": "pristine manifest mismatch"})

    def test_compiler_capacity_failures_are_infrastructure(self):
        for field in ["timed_out", "output_limit_exceeded", "cleanup_pending"]:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                workspace = Path(directory)
                task = coding.TASKS[0]
                coding.write_project(workspace, task["initial"])
                success = dict(exit_code=0, timed_out=False, output_limit_exceeded=False, stdout="", stderr="")
                failure = dict(success, **{field: True})
                with patch.object(coding, "run_oracle_process", side_effect=[success, failure]):
                    result = coding.oracle(workspace, task, 0)
                self.assertFalse(result["passed"])
                self.assertIn("infrastructure_error", result)


if __name__ == "__main__":
    unittest.main()
