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

Successful collection also writes `summary.json`: raw samples retain count,
minimum, median, p95 and maximum. Store aggregate rows retain only the supplied
statistics; each cold-new-inode measurement is explicitly one sample.
Store operational cases at 257 and 512 Sessions retain separate scenario keys;
missing operational measurements or duplicate aggregate keys fail admission.
Compatibility requires the same
schema, recipe, optimized build, OS/architecture, CPU model/available count and
toolchain; commit and runner instance names are provenance, not compatibility.
`trend.py OUTPUT --baseline DIRECTORY` compares only matching evidence.
An unreadable explicit local baseline is `baseline_rejected`; `lookup_failed`
is reserved for a failed remote lookup. CI looks
through at most ten successful default-branch runs of this same workflow/repository
and 100 artifacts per run, with a three-minute total budget. Its separate summary
artifact contains only metadata.json and summary.json; compressed and expanded
input are capped at 1 MiB and archives are read without filesystem extraction.
Missing, rejected and failed lookups are explicit states, never a no-regression
result. Job summaries show absolute and relative median deltas and distributions;
there is no uncalibrated timing threshold or automated mutation of baselines.

`cargo test --locked -p rsi-agent-executor --test end_to_end
report_cross_session_fairness -- --ignored --nocapture` emits four `FAIRNESS`
rows for active limits one/two, with and without blocked observation end hooks.
It records short-request provider wait and whether the slower Session remains
active. Run the owning pool/observation and parked-lane cancellation tests with
it; these separately prove reacquisition and cancellation invariants. The probe
uses isolated in-memory services and a controlled 100 ms release delay. It is
report-only evidence about this workload, not proof of global scheduling fairness.
