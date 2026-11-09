# rsi-shell-native

This ordinary provider composes Bash or PowerShell on native Linux and Windows.
`ShellKind` selects the real command syntax and identity: `bash` with producer
`rsi.coding.bash`, or `pwsh` with producer `rsi.coding.pwsh`. Neither kind invokes
an intermediate shell or a WSL launcher.

`ShellJobProducerFactory` and `ShellToolFactory` have separate leases. The
producer can be published before a Tool catalog stage, and retiring a catalog
does not retire already-owned Jobs. Both kinds share preparation, approval,
foreground/background dispatch, timeout, cancellation, capture and settlement.
The dialect owns command arguments, encoding and model instructions.

Construction receives an explicit absolute invocation path and environment.
No factory discovers executables or captures ambient environment at activation.
Sandbox admits the actual executable at preparation, preserving multicall
argv[0] and Windows application execution aliases. The environment is scrubbed
and its noninteractive defaults frozen once. RSI names, loader and shell hooks,
secret-shaped names and credential-bearing proxy URLs are removed. Non-UTF-8
names and proxy values are dropped; ordinary values remain byte-exact. Windows
drive-directory entries such as `=D:` are excluded.

Bash receives `--noprofile --norc -c <command>`. PowerShell receives
`-NoLogo -NoProfile -NonInteractive -Command <command>` with DSH's UTF-8 output
preamble. The exact Sandbox plan is used for every process. A Local lease seals
that explicit invocation and environment before approval; a target lease resolves
the selected shell's selector on the target. A prepared plan is single-use.
Execution never substitutes its program or environment after approval.

Scoped foreground and background work use the selected Jobs producer. Origin
comes from the executor's `JobOrigin`; display labels contain the approved
command with bounded length and sanitized control characters. Processes receive
the full command. Previews use bounded Process tail peeks. Unscoped foreground
calls run directly; background calls require live turn-scoped Jobs authority and
have no command timeout. Foreground timeout/cancellation joins native cleanup.
Windows owns descendants through a Job Object and force-terminates them rather
than using Unix signals.

Foreground model text includes status, exit code and signal, marks truncated
prefixes and replaces terminal control characters with U+FFFD. Structured stream
bytes and offsets remain authoritative. Nonzero exit is a normal command outcome
the model can correct. Both tools are Local-program Callable; internal calls
preserve approval, Sandbox policy and Turn Jobs ownership without granting a
separate background lifetime. Default tests exercise sealed Tool Runtime
behavior; native restricted-token acceptance is opt-in.
