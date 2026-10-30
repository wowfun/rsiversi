import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch
import run


class Evidence(unittest.TestCase):
    def setUp(self):
        # platform.platform() may itself invoke subprocess.run on Linux.
        platform = patch.object(run.platform, "platform", return_value="fixture-platform")
        platform.start()
        self.addCleanup(platform.stop)

    def test_untracked_source_marks_evidence_dirty(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root / 'source.rs').write_text('untracked source')
            original = run.capture
            def capture(*args):
                return original(*args) if args[:2] == ('git', 'status') else 'fixture'
            execute = subprocess.run
            def probe(args, **kwargs):
                if args[0] == 'git':
                    return execute(args, **kwargs)
                return type('Result', (), {'returncode': 7})()
            with patch.object(run, 'root', root), patch.object(run, 'capture', side_effect=capture), patch.object(run.subprocess, 'run', side_effect=probe):
                destination = root / 'report'
                run.main(['tui', str(destination)])
                self.assertTrue(json.loads((destination / 'metadata.json').read_text())['dirty'])

    def test_zero_tests_and_empty_reports_fail_despite_exit_zero(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(run, "capture", return_value="fixture"), patch.object(run.subprocess, "run") as probe:
                probe.return_value.returncode = 0
                self.assertEqual(run.main(["gui", str(root / "gui")]), 1)
                metadata = json.loads((root / "gui/metadata.json").read_text())
                self.assertTrue(all(result["evidence_error"] for result in metadata["results"]))
            (root / "cells.json").write_text('{"width":120,"height":40,"iterations_per_run":50,"runs":[]}')
            for surface in ("store", "tui"):
                with self.assertRaises(ValueError):
                    run.validate_evidence(surface, 0, root)

    def valid_reports(self, root):
        (root / "store").mkdir()
        cases = {f"cas-publication-{size}": ("cas",) for size in (64, 65536, 1048576)}
        cases.update({f"working-set-{count}": ("reader", "validation") for count in (257, 512)})
        cases["mixed-lanes"] = ("reader", "writer")
        for case, required in cases.items():
            (root / f"store/{case}.json").write_text(json.dumps({lane: [{"admission_ns": 0, "worker_ns": 0}] if lane in required else [] for lane in ("reader", "writer", "validation", "cas")}))
        projection = [dict(blocks=blocks, run=run, iterations=500, baseline_cpu_ns=0, cached_cpu_ns=0, unchanged_reparses=0) for blocks in (16, 64, 128) for run in range(10)]
        store = [f"case facts={count} payload={size} shape={shape} db_bytes=100" for count in (1000, 10000) for size in (256, 16384) for shape in ("LongOpen", "MixedSessions")]
        store += [f"metadata_case sessions={count} samples=100" for count in (128, 256, 512)]
        store += [f"control_case business_controls={count} terminal_markers=1" for count in (1000, 10000, 50000)]
        measured = []
        for heading in store:
            measured.append(heading)
            labels = ("warm_fast_open", "warm_first_session_validation", "warm_full_verify") if heading.startswith("case ") else ("reopened_header", "warm_complete_recent_catalog", "first_session_validation", "one_call_control_subtree_snapshot", "mixed_recent_read_and_append") if heading.startswith("metadata_case ") else ("cold_control_one_fact_page", "warm_control_one_fact_page")
            measured.extend(f"{label} min_ns=0 median_ns=0 p95_ns=0" for label in labels)
            if heading.startswith("case "): measured.append("cold_new_inode open_ns=0 first_validation_ns=0 verify_ns=0")
        store = measured
        (root / "probe-0.log").write_text("\n".join(store + ["PROJECTION_PERF " + json.dumps(row) for row in projection]))
        frames = [dict(detail_bytes=size, scenario=scenario, sample=sample, output_bytes=123, allocation_calls=0, allocation_requested_bytes=0) for size in (32, 65536) for scenario in ("unchanged", "single_block", "single_detail") for sample in range(10)]
        (root / "probe-1.log").write_text("\n".join("global_frame " + json.dumps(row) for row in frames))
        (root / "cells.json").write_text(json.dumps(dict(width=120, height=40, iterations_per_run=50, runs=[dict(blocks=blocks, run=run, cpu_ns=0, edit_to_cells_ns=[0]*50) for blocks in (16, 64, 128) for run in range(10)])))

    def test_complete_owner_formats_allow_zero_timings_and_unused_lanes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.valid_reports(root)
            for surface, index in (("store", 0), ("gui", 0), ("gui", 1), ("tui", 0)):
                run.validate_evidence(surface, index, root)

    def test_missing_and_duplicate_cases_fail(self):
        for surface, index, filename in (("store", 0, "store/working-set-257.json"), ("gui", 0, "probe-0.log"), ("gui", 1, "probe-1.log"), ("tui", 0, "cells.json")):
            for duplicate in (False, True):
                with self.subTest(surface=surface, index=index, duplicate=duplicate), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    self.valid_reports(root)
                    path = root / filename
                    if surface == "store":
                        if duplicate:
                            (root / "store/extra.json").write_text(path.read_text())
                        else:
                            path.unlink()
                    elif surface == "tui":
                        report = json.loads(path.read_text())
                        report["runs"] = report["runs"] + [report["runs"][0]] if duplicate else report["runs"][:1]
                        path.write_text(json.dumps(report))
                    else:
                        lines = path.read_text().splitlines()
                        path.write_text("\n".join(lines + [lines[-1]] if duplicate else lines[:-1]))
                    with self.assertRaises(ValueError):
                        run.validate_evidence(surface, index, root)

    def test_each_store_lane_and_workload_must_be_present(self):
        for filename, lane in (("mixed-lanes", "reader"), ("mixed-lanes", "writer"), ("working-set-257", "validation"), ("cas-publication-64", "cas")):
            with self.subTest(case=filename, lane=lane), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.valid_reports(root)
                path = root / f"store/{filename}.json"
                row = json.loads(path.read_text()); row[lane] = []
                path.write_text(json.dumps(row))
                with self.assertRaises(ValueError): run.validate_evidence("store", 0, root)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); self.valid_reports(root)
            path = root / "probe-0.log"; path.write_text("\n".join(path.read_text().splitlines()[1:]))
            with self.assertRaises(ValueError): run.validate_evidence("store", 0, root)

    def test_failed_probe_and_existing_output_are_never_accepted(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "failure"
            with patch.object(run, "capture", return_value="fixture"), patch.object(run.subprocess, "run") as probe:
                probe.return_value.returncode = 7
                self.assertEqual(run.main(["tui", str(root)]), 1)
                self.assertEqual(json.loads((root / "metadata.json").read_text())["results"][0]["exit_code"], 7)
                with self.assertRaises(FileExistsError):
                    run.main(["tui", str(root)])
                self.assertEqual(probe.call_count, 1)

    def test_corrupt_lane_rows_are_retained_as_evidence_errors(self):
        for corrupt in ([None], {"reader": [None]}, {"reader": "bad"}):
            with self.subTest(corrupt=corrupt), tempfile.TemporaryDirectory() as temporary:
                destination = Path(temporary) / "report"
                def probe(*args, **kwargs):
                    (destination / "store").mkdir()
                    (destination / "store/corrupt.json").write_text(json.dumps(corrupt))
                    return type("Result", (), {"returncode": 0})()
                with patch.object(run, "capture", return_value="fixture"), patch.object(run.subprocess, "run", side_effect=probe):
                    self.assertEqual(run.main(["store", str(destination)]), 1)
                metadata = json.loads((destination / "metadata.json").read_text())
                self.assertTrue(metadata["results"][0]["evidence_error"])

    def test_incomplete_work_and_invalid_timings_fail(self):
        for key, value in (("cpu_ns", -1), ("edit_to_cells_ns", [0]*49)):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); self.valid_reports(root)
                path = root / "cells.json"; report = json.loads(path.read_text())
                report["runs"][0][key] = value; path.write_text(json.dumps(report))
                with self.assertRaises(ValueError): run.validate_evidence("tui", 0, root)

    def test_workload_headings_without_samples_are_not_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); self.valid_reports(root)
            path = root / "probe-0.log"
            path.write_text(path.read_text().replace("warm_fast_open min_ns=0 median_ns=0 p95_ns=0", "warm_fast_open samples=0", 1))
            with self.assertRaisesRegex(ValueError, "operation samples"):
                run.validate_evidence("store", 0, root)
