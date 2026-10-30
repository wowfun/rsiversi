"""Run one bounded parser campaign with temporary corpus and retained provenance."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

FIXTURE = Path(__file__).resolve().parent
ROOT = FIXTURE.parents[2]
TOOLCHAIN = "nightly-2026-10-24"
FUZZ_VERSION = "0.13.2"
LIMITS = {"apply_patch": 2_000_001, "mcp_frame": 2_097_155}


def capture(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def run(target, destination, fixture=FIXTURE):
    output = destination.resolve()
    output.mkdir(parents=True, exist_ok=False)
    artifact = output / "failure.bin"
    lock = fixture / "Cargo.lock"
    original_lock = lock.read_bytes()
    metadata = {"format": 1, "target": target, "max_input_bytes": LIMITS[target],
                "max_total_time_seconds": 60, "results": {}}
    report = output / "metadata.json"
    report.write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    status = 1
    try:
        seeds = sorted((fixture / "corpus" / target).iterdir())
        if not seeds or any(not path.is_file() or path.stat().st_size > LIMITS[target] for path in seeds):
            raise ValueError("campaign requires nonempty, bounded regular seed files")
        fuzz_version = capture("cargo", "fuzz", "--version")
        if fuzz_version != f"cargo-fuzz {FUZZ_VERSION}":
            raise ValueError(f"expected cargo-fuzz {FUZZ_VERSION}, found {fuzz_version}")
        metadata.update(commit=capture("git", "rev-parse", "HEAD"),
                        dirty=bool(capture("git", "status", "--porcelain", "--untracked-files=all")),
                        rust=capture("rustc", f"+{TOOLCHAIN}", "-Vv"), cargo_fuzz=fuzz_version,
                        platform=platform.platform(), runner=os.environ.get("RUNNER_NAME", "local"),
                        lock_sha256=hashlib.sha256(original_lock).hexdigest(), seed_count=len(seeds))
        with tempfile.TemporaryDirectory(prefix=f"{target}-corpus-", dir=output.parent) as generated:
            command = ["cargo", f"+{TOOLCHAIN}", "fuzz", "run", "--fuzz-dir", str(fixture),
                       "--jobs", "1", target, generated, str(fixture / "corpus" / target), "--",
                       "-max_total_time=60", "-timeout=60", f"-max_len={LIMITS[target]}",
                       f"-exact_artifact_path={artifact}"]
            metadata["command"] = command
            report.write_text(json.dumps(metadata, indent=2), encoding="utf-8")
            result = subprocess.run(command, cwd=ROOT)
            status = result.returncode
            metadata["results"]["exit_code"] = status
        if lock.read_bytes() != original_lock:
            raise ValueError("cargo-fuzz changed the fixture lockfile")
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        metadata["results"]["error"] = str(error)
        status = status or 1
    finally:
        if artifact.exists():
            if artifact.stat().st_size > LIMITS[target]:
                artifact.unlink()
                metadata["results"]["error"] = "failure input exceeds the target limit"
                status = status or 1
            # libFuzzer also writes slow inputs to exact_artifact_path on success.
            elif status == 0:
                artifact.unlink()
        metadata["results"]["failure_input"] = artifact.name if artifact.exists() else None
        report.write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    return status


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=tuple(LIMITS))
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    raise SystemExit(run(args.target, args.destination))
