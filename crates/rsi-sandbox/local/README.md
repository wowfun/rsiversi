# rsi-sandbox-local

This ordinary plugin probes only explicit absolute candidate paths. A
bubblewrap candidate must create the same namespace and read-only-root shape
used by restricted plans and propagate a reserved child exit code; an
executable that merely exits successfully is not enforcement evidence.
Landlock runners implement `--rsi-landlock-probe 23` and return exit code 23
only after their own enforcement probe succeeds. Probes are bounded by a
two-second timeout and do not consult `PATH`.

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
workspace exactly equal to `/tmp` or `/` is invalid for every restricted
backend; root authority is outside a workspace-scoped sandbox contract, and a
Bubblewrap root rebind would additionally hide its private scratch and device
mounts. A canonical descendant
such as `/tmp/work` is rebound after tmpfs creation in either restricted mode.
Landlock plans retain host scratch and therefore do not claim Bubblewrap's
private-scratch evidence.

Default tests inject the probe and inspect plans without changing host policy.
On a Linux host with `/usr/bin/bwrap` and user-namespace support, the ignored
`native_bubblewrap_enforces_read_only_and_workspace_write_plans` test may be
run explicitly; it executes generated plans and proves the workspace write
boundary. It does not claim network isolation, Landlock, Windows, or macOS.
