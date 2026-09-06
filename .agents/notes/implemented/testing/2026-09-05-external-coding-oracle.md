---
name: Parent-owned coding evaluation and isolated fixture execution
---

## Problem

A tested library can exit its process before assertions execute. A completion
receipt compiled into the same process is also forgeable: admitted Rust can run
a constructor, inspect environment and build sources, write the receipt, and
exit successfully. Neither outcome establishes behavioral correctness. Process
groups alone also fail to contain a descendant that creates a new session.

## Decision

The evaluator's parent owns expected values and compares the bounded outputs of
an external function runner. The runner receives inputs and calls the public
fixture functions; it contains neither expected values nor a success credential.
Compilation uses private working, HOME, CARGO_HOME and target directories.
Its own mount namespace includes only the fresh project, read-only toolchain
and system files. Private HOME alone does not exclude Cargo configuration in
ancestors of the working directory; those ancestors contain no host files in
the compilation namespace.
Linux Bubblewrap provides fresh PID, mount, user and network namespaces for
execution, with a read-only executable and runtime files and private temporary
storage. The parent's source, build directory, environment and reports remain
outside the fixture namespace. An unavailable sandbox fails grading explicitly.

The measured Agent has a separate outer PID and filesystem namespace containing
only its private task state/workspace, executable, system runtime and toolchain.
A workspace-write Tool inside that namespace cannot read the repository,
harness, reference implementations, key file, user HOME or prior reports.
The Agent retains network access for its provider and receives the key only
through its environment; nested Tool confinement and environment scrubbing
remain the product's responsibility. An outer namespace is necessary because
making the host root read-only still exposes scoring authority and credentials.
The same process supervisor bounds Agent output and lifetime, and the namespace
contains descendants that leave their process group. Namespace preflight must
succeed before the Agent launches.

This decision partially supersedes the receipt mechanism in the
[CLI coding workflow note](../feature/2026-09-05-cli-coding-workflow.md).
The [eval contract](../../../../crates/rsi/core/eval/README.md) owns concrete
limits, task semantics and invocation behavior. Initial infrastructure failure
prevents provider invocation; later grading failure preserves attempt evidence
with its infrastructure classification. Test cases exercise constructor forgery,
malformed trace projections and infrastructure-versus-budget precedence,
ambient Cargo configuration, output limits, timeout, setsid descendants and
report persistence. They use dummy credentials and never call a provider.

## Alternatives considered

Banning constructor attributes alone would leave other filesystem and process
introspection paths available to admitted Rust. Random receipt constants and
success-looking output are still generated inside the process being measured.
Host-side comparison with process isolation removes that shared scoring authority.
An unconfined fallback would invalidate the isolation tests precisely when the
required execution boundary is unavailable, so the evaluator rejects that run.

## Consequences

The oracle requires native Linux Bubblewrap support. It evaluates only the
supplied finite behavioral cases, with no claim of arbitrary program equivalence
or protection against a compromised compiler or operating-system kernel. A
timeout bounds reporting while separately owned cleanup reaps a direct child
that remains in uninterruptible I/O. The supervisor observes exit with WNOWAIT
and signals the process group before reaping its leader, retaining PID identity
until the last signal. The runner preserves old reports as evidence of
their original oracle and binary and refuses existing output directories; a correction does not justify resampling a
valid failed coding attempt.

Preservation is not filesystem immutability against the same user. Read-only
mode bits would remain reversible by that owner; the measured Agent instead
never receives a mount of the report directory. Source admission is deliberately
a conservative lexical subset for the fixed tasks: include/cfg names are
rejected even in imports, comments or strings, and path attributes are rejected
with inner syntax or intervening comments. This is not a general Rust parser.
