---
name: Session read retirement retains actual work ownership
---

## Problem

A client dropping an export stream does not prove the Store worker has stopped.
Purging after merely dropping the observation can race a live canonical read.

## Decision

Session read admission owns both an observation cancellation signal and the
actual work lease. Retirement cancels observation, emits one typed shutdown error
then EOF, and waits for real work ownership before Store deletion. The
[Session contract](../../../../crates/rsi/session/README.md) owns the drain and
response deadlines. Purge target reads authorize the origin and execution
location before exposing selection diagnostics.
An abandoned controlled-work reporter has no in-process settlement path. Kernel
returns UnsettledWork and Session projects UncertainWork in purge previews,
separately from live Busy owners. This adds a Session API v6 reply variant.

## Alternatives considered

Silent EOF looks like successful exhaustion. Releasing the real lease when a
consumer drops its stream permits destructive maintenance to race cleanup.
Treating every shutdown as Busy incorrectly blames an unrelated selected Session.

## Consequences

A caller can stop receiving before native work settles. Capacity refusal during
drain is separate from Session-specific Busy. A finite group drain deadline is
separate from the mutation response deadline; an uncertain reply retains actual
work ownership and the durable receipt remains the reconciliation path. Integration
tests race an open export with purge and verify error-then-EOF and delayed deletion.
Uncertain controlled work requires confirmed native cleanup and a Service Host
restart; neither dropping observations nor elapsed time supplies that proof.
