# rsi-shell-bash

When Tool execution supplies a Jobs scope, foreground Bash also uses the named
Bash producer. Foreground waits and cancellation retain their existing result
classification; the submitted origin comes from the executor's `JobOrigin`
extension. UI previews use the producer's bounded Process tail peek. Calls
without a scope execute directly and have no live Jobs preview.

This package is the ordinary Linux Bash capability plugin. It has two explicit
owners:

- `BashJobProducerFactory` registers the stable `rsi.coding.bash` producer with
  process-local Jobs and delegates every admitted process to Process.
- `BashToolFactory` registers only the model-facing `bash` definition through
  `ToolRegistrarContract`. It submits background work to that stable producer
  and submits scoped foreground work to the same producer. Both retain output
  until their owning tool reports it; standalone foreground calls use Process.

The factories intentionally have separate leases and activation dependencies.
Compositions may therefore publish the producer before beginning a Tool
catalog stage, while retiring a catalog never silently retires already-owned
Jobs. Neither factory discovers Bash nor captures ambient environment during
activation: construction requires an explicit canonical executable and a
frozen environment snapshot. The snapshot is scrubbed case-insensitively for
RSI names, loader and shell hooks, secret-shaped names, and credential-bearing
proxy URLs; non-UTF-8 names and proxy values are dropped while ordinary raw
values remain byte-exact.

Each command runs as `bash --noprofile --norc -c <command>` through the exact
Sandbox-produced Process plan. Foreground timeout/cancellation joins process
cleanup. Model-facing foreground text labels every retained tail whose prefix
was truncated and replaces terminal control characters with U+FFFD; the
structured stream text and offsets remain authoritative.
Foreground model text always includes status, exit code, and signal, including
when output is nonempty. A nonzero exit is a command outcome rather than a Tool
transport failure, so the model can inspect the evidence and correct its work.
Background
commands have no command timeout and require live
turn-scoped Jobs authority. Activation fails closed outside Linux until the
Process family provides equivalent native settlement semantics there.
