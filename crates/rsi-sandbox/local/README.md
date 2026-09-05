# rsi-sandbox-local

This ordinary plugin probes only explicit absolute candidate paths. A
bubblewrap candidate must create the same namespace and read-only-root shape
used by restricted plans and propagate a reserved child exit code; an
executable that merely exits successfully is not enforcement evidence.
Landlock runners implement `--rsi-landlock-probe 23` and return exit code 23
only after their own enforcement probe succeeds. All executable behavior probes
in one activation share a cumulative two-second execution budget and do not
consult `PATH`. Candidate staging is separately byte-bounded and does not
consume that behavior budget; copying from a pinned regular-file handle is
blocking filesystem work and is not falsely described as having a hard
wall-clock deadline.

Optional factory activation publishes a service even when no backend passes;
restricted calls then fail closed while `danger-full-access` remains an explicit
unconfined holder bypass. A factory constructed with required restricted
support instead fails activation before publishing the service. This
construction policy is deliberately outside serializable Profile configuration,
so a Profile replacement cannot disable a composition-owned readiness
requirement. A required activation distinguishes exhaustion of the shared
behavior-probe budget from ordinary candidate rejection in its failure.

The selected wrapper is frozen for the generation. Restricted requests produce
wrapper argv and a matching stamp; absence fails closed. The durable stamp
identifies the staged wrapper bytes by SHA-256 rather than its temporary path.
The Landlock backend
is an external safe runner because applying Landlock between fork and exec
inside this `unsafe_code = deny` library would require an unsafe pre-exec hook.

Each candidate is first copied into a generation-private temporary directory;
the feature probe and every later plan execute that same owned copy. Replacement
of the configured source pathname during or after probing therefore cannot
change the selected wrapper. Wrapper argv uses native `OsString` values so valid non-UTF-8 Unix
program, cwd, and workspace paths are preserved rather than rewritten.
Staged wrappers are deliberately non-privileged. A bubblewrap installation
that requires a setuid executable is rejected rather than copying elevated
mode bits into application-owned storage.

Bubblewrap restricted plans create a private tmpfs `/tmp` before rebinding the
canonical workspace read-only or writable according to the requested mode. A
workspace whose live canonical identity is the system temporary root or
filesystem root is invalid for every restricted
backend; root authority is outside a workspace-scoped sandbox contract, and a
Bubblewrap root rebind would additionally hide its private scratch and device
mounts. A canonical descendant
such as `/tmp/work` is rebound after tmpfs creation in either restricted mode.
Landlock plans retain host scratch and therefore do not claim Bubblewrap's
private-scratch evidence.

Default tests inject the probe and inspect plans without changing host policy.
On Linux, a composition that selects required restricted support must provide a
working configured backend such as `/usr/bin/bwrap` with usable user namespaces;
activation intentionally fails before publication when that prerequisite is
absent.
On a Linux host with `/usr/bin/bwrap` and user-namespace support, the ignored
`native_bubblewrap_enforces_read_only_and_workspace_write_plans` test may be
run explicitly; it executes generated plans and proves the workspace write
boundary, zero effective capabilities, a minimal `/dev`, host-path write
denial, and a fresh PID-namespace `/proc`. Explicitly selecting this ignored
test requires its native dependencies; missing prerequisites fail the gate.
It does not claim network isolation, Landlock, Windows, or macOS.
