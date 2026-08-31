# rsi-tools

This ordinary plugin owns one process-wide Tool catalog provider. A caller
opens a bounded unpublished stage, contributes exact-name definitions through
its registrar, and consumes the stage to seal one immutable runtime. Duplicate
names fail atomically, schema listing is deterministic, and runtime execution
never shares the staging lock or holds provider locks across Tool code. One
stage admits at most 64 definitions, and a registration's cooperative timeout
must be within 1..=600,000 milliseconds.

Registration leases support activation rollback by withdrawing their exact
batch while the stage is open. Once the stage is sealed, dropping or retiring
a contributor lease cannot change definitions, invalidate prepared calls, or
cancel admitted calls. The immutable catalog owns that generation until it is
dropped; provider teardown owns the bounded cancel-and-join path.

Timeout and caller cancellation cancel a child token and then wait for the
tool future to settle before returning. A body-owned result or non-cancellation
error that settles after that signal remains authoritative, because it can
carry exact evidence of already-applied effects; only a cooperative
`Cancelled` outcome is classified by the Runtime as cancellation or timeout.
This preserves quiescence but cannot preempt a trusted tool that ignores
cooperative cancellation.

A trusted Tool body returns a bounded typed result. Settlement attaches the
collected enforcement stamps and validates that combined result once before it
enters retained state; the provider does not immediately rewalk and
re-serialize the identical value a second time.
Settled retained values are shared internally: provider synchronization only
clones the shared owner, while the owned protocol value requested by `query`
or `wait` is materialized after the provider lock is released.

Starting a prepared call transfers settlement to the provider-wide result
owner while the immutable runtime pins its exact catalog generation.
Dropping the caller's waiter does not abandon the body or its retained outcome.
Retained-result waiters register their notification before querying the
snapshot, so settlement cannot be lost between observation and suspension.
Tool-body panics and destruction of their panic payloads use separate unwind
boundaries; a recursively panicking payload becomes a retained execution
failure instead of terminating the provider-owned settlement task.
An orchestrator retires a settled outcome only after its own durable evidence
is committed. Dropping the catalog cancels its active calls, removes its
settled retained entries immediately, and discards any outcome that settles
after catalog authority has disappeared.

Provider cleanup cancels active calls and joins them for
`shutdown_timeout_ms` (default 10 seconds, maximum 5 minutes). A body that does
not settle within that interval is reported as unresolved cleanup; safe Rust
cannot forcibly stop arbitrary trusted Tool code.
