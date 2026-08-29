# rsi-tools

This ordinary plugin owns one exact-name tool registry. Duplicate active names
fail, schema listing is deterministic, and execution snapshots an `Arc` before
awaiting so registry locks never cross tool code.
One generation admits at most 64 active definitions, and a registration's
cooperative timeout must be within 1..=600,000 milliseconds.

Timeout and caller cancellation cancel a child token and then wait for the
tool future to settle before returning. This preserves quiescence but cannot
preempt a trusted tool that ignores cooperative cancellation.

Starting a prepared call transfers settlement to the Tool Runtime generation.
Dropping the caller's waiter does not abandon the body or its retained outcome.
An orchestrator retires a settled outcome only after its own durable evidence
is committed.

Generation cleanup cancels active calls and joins them for
`shutdown_timeout_ms` (default 10 seconds, maximum 5 minutes). A body that does
not settle within that interval is reported as unresolved cleanup; safe Rust
cannot forcibly stop arbitrary trusted Tool code.
