"""Run owning report-only probes and preserve raw distributions and provenance."""
import json
import argparse
import os
import re
from pathlib import Path
import platform
import subprocess
import sys

root = Path(__file__).resolve().parents[3]

def capture(*args):
    return subprocess.check_output(args, cwd=root, text=True).strip()

COMMANDS = {
    "store": [["cargo", "bench", "--locked", "-p", "rsi-agent-store-sqlite", "--features", "test-support", "--bench", "store_scaling"]],
    "gui": [
        ["cargo", "test", "--locked", "--release", "-p", "rsi-gui", "--lib", "projection_performance", "--", "--ignored", "--nocapture", "--test-threads=1"],
        ["cargo", "test", "--locked", "--release", "-p", "rsi-gui", "--features", "test-support", "--test", "submission", "measure_actual_frames", "--", "--ignored", "--nocapture", "--test-threads=1"],
    ],
    "tui": [["cargo", "test", "--locked", "--release", "-p", "rsi-terminal-ui", "--lib", "deterministic_render_cost", "--", "--ignored", "--nocapture", "--test-threads=1"]],
}

def require_matrix(rows, keys, expected):
    if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
        raise ValueError("Invalid measurement rows")
    identities = [tuple(row[key] for key in keys) for row in rows]
    if len(identities) != len(expected) or set(identities) != expected:
        raise ValueError(f"Incomplete or duplicate measurement matrix: {keys}")


def nonnegative_samples(rows, keys):
    if any(not isinstance(row, dict) or type(row.get(key)) is not int or row[key] < 0
           for row in rows for key in keys):
        raise ValueError(f"Invalid measurements: {keys}")


def validate_store(output):
    cases = {f"cas-publication-{size}": ("cas",) for size in (64, 65536, 1048576)}
    cases.update({f"working-set-{count}": ("reader", "validation") for count in (257, 512)})
    cases["mixed-lanes"] = ("reader", "writer")
    paths = {path.stem: path for path in (output / "store").glob("*.json")}
    if set(paths) != set(cases):
        raise ValueError("Incomplete Store case matrix")
    for case, required in cases.items():
        row = json.loads(paths[case].read_text(encoding="utf-8"))
        if not isinstance(row, dict):
            raise ValueError("Invalid Store measurement row")
        for lane in ("reader", "writer", "validation", "cas"):
            samples = row.get(lane)
            if not isinstance(samples, list) or (lane in required and not samples):
                raise ValueError(f"Missing {case}.{lane} measurements")
            nonnegative_samples(samples, ("admission_ns", "worker_ns"))
    log = (output / "probe-0.log").read_text(encoding="utf-8")
    for pattern, expected, labels in (
        (r"^case facts=(\d+) payload=(\d+) shape=(\w+) db_bytes=", {(str(count), str(size), shape) for count in (1000, 10000) for size in (256, 16384) for shape in ("LongOpen", "MixedSessions")}, ("warm_fast_open", "warm_first_session_validation", "warm_full_verify")),
        (r"^metadata_case sessions=(\d+) ", {"128", "256", "512"}, ("reopened_header", "warm_complete_recent_catalog", "first_session_validation", "one_call_control_subtree_snapshot", "mixed_recent_read_and_append")),
        (r"^control_case business_controls=(\d+) ", {"1000", "10000", "50000"}, ("cold_control_one_fact_page", "warm_control_one_fact_page")),
    ):
        found = re.findall(pattern, log, re.MULTILINE)
        if len(found) != len(expected) or set(found) != expected:
            raise ValueError("Incomplete or duplicate Store workload matrix")
        for match in re.finditer(pattern, log, re.MULTILINE):
            block = re.split(r"^(?:case |metadata_case |control_case |operational_case )", log[match.end():], maxsplit=1, flags=re.MULTILINE)[0]
            for label in labels:
                if not re.search(rf"^{label} (?:samples=[1-9]\d* )?min_ns=\d+ median_ns=\d+ (?:max_ns|p95_ns)=\d+$", block, re.MULTILINE):
                    raise ValueError(f"Missing Store operation samples: {match.groups()} {label}")
            if labels[0] == "warm_fast_open" and not re.search(r"^cold_new_inode open_ns=\d+ first_validation_ns=\d+ verify_ns=\d+$", block, re.MULTILINE):
                raise ValueError("Missing copied-inode Store measurements")


def validate_evidence(surface, index, output):
    if surface == "store":
        validate_store(output)
    elif surface == "gui":
        marker = "PROJECTION_PERF " if index == 0 else "global_frame "
        rows = [json.loads(line.split(marker, 1)[1]) for line in (output / f"probe-{index}.log").read_text(encoding="utf-8").splitlines() if marker in line]
        if index == 0:
            require_matrix(rows, ("blocks", "run"), {(blocks, run) for blocks in (16, 64, 128) for run in range(10)})
            nonnegative_samples(rows, ("iterations", "baseline_cpu_ns", "cached_cpu_ns", "unchanged_reparses"))
            if any(row["iterations"] != 500 or row["unchanged_reparses"] != 0 for row in rows):
                raise ValueError("Incomplete GUI projection work")
        else:
            require_matrix(rows, ("detail_bytes", "scenario", "sample"), {(size, scenario, sample) for size in (32, 65536) for scenario in ("unchanged", "single_block", "single_detail") for sample in range(10)})
            nonnegative_samples(rows, ("output_bytes", "allocation_calls", "allocation_requested_bytes"))
            if any(row["output_bytes"] == 0 for row in rows):
                raise ValueError("GUI probe measured no work")
    else:
        report = json.loads((output / "cells.json").read_text(encoding="utf-8"))
        if any(report[key] != expected for key, expected in (("width", 120), ("height", 40), ("iterations_per_run", 50))):
            raise ValueError("Incorrect terminal workload")
        rows = report["runs"]
        require_matrix(rows, ("blocks", "run"), {(blocks, run) for blocks in (16, 64, 128) for run in range(10)})
        nonnegative_samples(rows, ("cpu_ns",))
        if any(not isinstance(row.get("edit_to_cells_ns"), list) or len(row["edit_to_cells_ns"]) != 50 or any(type(sample) is not int or sample < 0 for sample in row["edit_to_cells_ns"]) for row in rows):
            raise ValueError("Incomplete terminal edit samples")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("surface", choices=tuple(COMMANDS))
    parser.add_argument("destination", help="new evidence directory")
    args = parser.parse_args(argv)
    surface, output = args.surface, Path(args.destination).resolve()
    output.mkdir(parents=True, exist_ok=False)
    commands = COMMANDS[surface]
    env = os.environ.copy()
    for key in tuple(env):
        if key.startswith("RSI_STORE_BENCH_"):
            del env[key]
    env.update(RSI_STORE_BENCH_EVIDENCE=str(output / "store"), RSI_STORE_BENCH_COUNTS="1000,10000", RSI_TUI_PERFORMANCE_REPORT=str(output / "cells.json"))
    metadata = dict(format=1, surface=surface, commit=capture("git", "rev-parse", "HEAD"), dirty=bool(capture("git", "status", "--porcelain", "--untracked-files=normal")), rust=capture("rustc", "-Vv"), cargo=capture("cargo", "-V"), platform=platform.platform(), runner=os.environ.get("RUNNER_NAME", "local"), cpu_count=os.cpu_count(), commands=commands, timing_gate=False, results=[])
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    for index, command in enumerate(commands):
        with (output / f"probe-{index}.log").open("w", encoding="utf-8") as log:
            result = subprocess.run(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        evidence_error = None
        if result.returncode == 0:
            try:
                validate_evidence(surface, index, output)
            except (ValueError, OSError, KeyError, TypeError) as error:
                evidence_error = str(error)
        metadata["results"].append(dict(exit_code=result.returncode, log=f"probe-{index}.log", evidence_error=evidence_error))
        (output / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    return int(any(result["exit_code"] or result.get("evidence_error") for result in metadata["results"]))


if __name__ == "__main__":
    sys.exit(main())
