# Repeatable performance evidence

The weekly/manual `performance-evidence` workflow runs optimized Linux Store,
GUI and terminal jobs with separate Cargo caches. Its independent parser-fuzz
job checks the locked fixture workspace and runs the bounded campaigns described
by the [parser fixture](../parser-fuzz/README.md).
`python3 fixtures/rsi/performance/run.py store|gui|tui OUTPUT` runs the
same selection locally from the repository root. It records the exact commit,
dirty-state flag, toolchain, runner/platform, commands and raw sample logs.
The Store reports queue/worker/CAS and cold-validation measurements; GUI reports
thread CPU, allocations and frame materialization; terminal reports thread CPU
and edit-to-TestBackend-cell samples, excluding physical paint and terminal I/O.

Elapsed time is report-only. Correctness, reconstruction and bounded-work
assertions still fail the job; measurements from different runner classes are
not directly comparable. Evidence is retained even on failure. No performance
claim follows from compilation or one uncontrolled wall-clock measurement.
Each invocation requires a new output directory; CI uses the job's temporary
directory outside Cargo caches. Successful probes must supply measurements for
the complete selected matrix; a zero-test exit is not evidence. Store requires
CAS publications at 64 B, 64 KiB and 1 MiB, mixed reader/writer lanes, and cold
reader/validation lanes for 257 and 512 Sessions. Its log must include measured operations for metadata
at 128/256/512 Sessions, controls at 1,000/10,000/50,000, and both history shapes
at 1,000/10,000 Facts with 256 B/16 KiB payloads. Empty unused lanes are valid.
GUI projection requires ten runs at each of 16/64/128 blocks (500 iterations);
frame evidence requires ten samples for each unchanged/block/detail scenario
at 32 B and 64 KiB. Terminal evidence requires the same thirty block/run pairs,
each with fifty edit-to-cell samples at 120 by 40 cells. Missing or duplicate
case identities fail validation; inherited Store matrix overrides are cleared.

Measurement rows must have the owning schema and nonzero work counts. Zero CPU
or queue durations remain valid samples on coarse timers; they are not a timing gate.

The admission tests run through `python3 fixtures/tools/python-tests.py
fixtures/rsi/performance` in ordinary CI as well as before measurement collection.
They isolate platform discovery from mocked subprocess execution, so Linux and
Windows exercise the same failed-probe and incomplete-evidence behavior.
