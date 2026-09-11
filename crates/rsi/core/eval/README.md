# Coding evaluation

`python3 crates/rsi/core/eval/coding.py --self-test` checks that each initial
fixture fails and the reference repair passes the external Rust oracle, then
exercises source-admission rejection. It needs no key or service. The evaluator
also runs Linux `/proc` timeout checks and a report-writing fixture with a dummy
credential; these checks never contact a provider. Scripted provider tests
remain separate evidence of the execution pipeline.

`python3 crates/rsi/core/eval/coding.py --live --key-file .local/dev/.env
--output .local/notes/0905/live` uses the built
`target/debug/rsi` binary and a fresh isolated Host per task. The key reader
accepts only the named literal `DEEPSEEK_API_KEY` assignment and never sources
the file or writes the key into task inputs or reports. Model selection uses
`--model`, then `DEEPSEEK_MODEL`, then `deepseek-flash`, in that order. The exact
selected name configures both the provider profile and the Session default and
is recorded in each report; a provider rejection never selects another model.
Model names are quoted
as one TOML table key, preserving dots and other literal model-name characters. Real integrations are
explicitly opt-in; ordinary Cargo tests never execute this script's live mode.

Evaluation version 10 covers a UTF-8 truncation repair, a two-module word-frequency
feature, and interval merging with a second requirement after restarting and
resuming the Session. Each task has one attempt, an eight-minute outer deadline,
and 48 Tool calls. Each Turn has 32 provider attempts: a single-turn task has
32 total, and the two-turn task has 64 total. The two-turn task splits only the
Tool quota equally between its immutable Turn settings. Reports record the
task-wide totals and the actual per-Turn settings. There is no task resampling.

The Agent process also runs in a fresh Bubblewrap PID and mount namespace.
Only its private task directory, the built executable, read-only system files
and toolchain are mounted. The repository, harness, expected answers, key file,
reports and real user HOME are absent. Cargo children receive an explicit
`CARGO_BUILD_JOBS=2` alongside offline dependency access. A private HOME and Cargo environment
exclude ambient configuration. Network access supports the provider; the key
is supplied only in the Agent environment and product Tools scrub it from
their children. Nested restricted Tool sandboxes remain enabled. The outer
namespace contains descendants even if they create a new process session.
Agent launch failure is an infrastructure error; execution never falls back
to a process outside this boundary.
Agent stdout/stderr each have a separate 128 MiB operational bound, allowing
JSONL envelopes around the 64 MiB generated-Fact budget. This bound differs
from the oracle's 1 MiB streams; exceeding it invalidates the run as an
infrastructure failure and preserves the truncated evidence.

The oracle copies only allowlisted regular source files into a fresh project,
uses a fixed manifest, lockfile, and function runner, and runs Cargo offline with
isolated build, working, HOME and CARGO_HOME directories. Compilation also runs
inside Bubblewrap: only the fresh build project, read-only toolchain and system
files are mounted, so ancestor Cargo configuration is absent. It rejects source symlinks, extra Rust modules,
build scripts, Cargo configuration/manifest changes, test-only compilation,
external includes, and unbounded source files. The editable workspace's test
results and model's success claims do not decide the score. Oracle source and
checks remain outside the editable workspace, and their hashes are recorded.
The parent Python process sends bounded function inputs and compares every
returned value to its own expected result. The Rust runner contains neither
expected answers nor a success receipt; zero exit and fixture-generated success
messages cannot establish a pass. Linux Bubblewrap runs the fixture in fresh
PID, mount, user and network namespaces, with only read-only system runtime files
and the executable, private temporary storage, and no Host or compiler environment.
The parent, source workspace, build project and reports are outside this namespace.
Child `setsid` cannot escape its PID namespace. Missing or unusable Bubblewrap is
an infrastructure failure; grading never falls back to unconfined execution.
An initial oracle infrastructure failure stops the task before any provider
invocation. A grading infrastructure failure is recorded separately from an
incorrect implementation and preserves the attempt's reports.
Compilation, sandbox preflight and execution share a 30-second deadline; retained
stdout and stderr are each bounded to 1 MiB. Timeout or output overflow stops
the owned process group and returns structured evidence for the report writer.
The supervisor signals the group before reaping its leader, retaining the PID
identity through cleanup. Malformed trace projections are infrastructure failures
and preserve stage evidence and reports; a later budget marker cannot downgrade
an infrastructure failure.
This evaluates the supplied behavioral cases, not arbitrary program equivalence
or protection against a compromised compiler or operating-system kernel.
Timeout tests observe the supervised subtree from the parent namespace and
verify that a fixture descendant calling `setsid` also stops; fixtures never
need writable access to Host-side evidence files.

Reports retain actual model snapshots, usage, resource counts, elapsed time,
durable Facts and controls, source diff, and oracle output. Outcomes distinguish
pass, behavioral failure, Agent failure, budget exhaustion, and infrastructure
failure. A passing smoke task establishes only the behavior of that task and
configuration, not general coding capability.

The oracle admits formatting and reordering of the three fixed module
declarations. The self-test runs ordinary Cargo test and format commands in
the editable fixture to prove they preserve admission. The lockfile includes
Cargo's canonical generated header. Reports preserve bounded source evidence
even when admission fails. Correcting an oracle may regrade retained evidence
offline; this must keep the original report and identify the changed oracle.
A terminal Agent or budget failure takes precedence over a subsequent source
admission failure. `--task` selects a subset for explicit diagnostic runs; it
does not authorize replacing a valid failed attempt with a better sample.
Report preservation is a runner policy: an existing output directory is
rejected and prior reports are never rewritten by a new run. These files are
not tamper-proof against their owning user or a compromised Host.
