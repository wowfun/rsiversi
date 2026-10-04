---
name: Bounded validation and shared context admission
---

## Problem

Cold validation retains one lane through a history-sized scan after waiter loss.
Context cursors survive execution-lane parking, while request and checkpoint
copies have no shared aggregate admission. Per-record bounds do not cover either
operation queues or the combined ownership lifetime.

## Decision

Cold callers and jobs have independent admission, selected-session proof work is
shared and survives waiter deadlines until proof publication or failure. Private
one-shot read validation stops after its waiter leaves. The
blocking job keeps the Store owner through progress-hook removal, transaction
settlement and result publication.
A successful read commit produces a valid proof despite later waiter loss.
ValidationBusy is a known pre-write refusal; Kernel must preserve that evidence.
The transient-refusal fencing policy is superseded by the
[retry-classification decision](../bug-fix/2026-10-03-transient-store-refusal-retry.md).
Admission refusal never fabricates a successful resume or drops an admitted native
attempt.

One service-level Context budget follows retained state and transient projections,
including parked claims, old generations and maintenance. Requests and checkpoint
buffers carry opaque admission ownership into the actual prepared or blocking job.
Checkpoint reads transfer deferred admission rather than reserving the format
maximum while queued. Validated exact-length admission occurs within the Store's
read snapshot, keeping allocation authority alive after caller cancellation.
Immutable composition pins cache borrowed Tool-catalog weights; request validation
caches canonical weights and projection reuses its caller's admission. This avoids
repeated full-catalog accounting while preserving admission before schema copies.
Construction workspace shrinks after options are built, but the frozen options
and each independently copied request keep separate ownership credits. Dropping
the options credit at provider dispatch would hide the copy needed for recovery.
Admission fails immediately on pressure so parked parents cannot wait for children
that require the same credit. A failed admitted Fact drops mutable cursor state and
releases its workspace; the invalid cursor cannot retry ingestion or project a
partial state. Blindly restoring the previous charge would retain partially
mutated state without its allocation credit. The current contracts belong
to the [Store](../../../../crates/rsi-agent/store-sqlite/README.md),
[Context](../../../../crates/rsi-agent/context/README.md), and
[API](../../../../crates/rsi-api/README.md) owners.

## Alternatives considered

Outer async timeouts leave native scans running. Whole-SSH retries can replay
accepted effects. Context quotas per executor generation duplicate available
credit. Parked-cursor eviction needs a separately specified exact replay policy.
Incremental history proof and active validation fairness require a stable-prefix
design and are outside the chosen first implementation.

## Consequences

Cooperative cancellation cannot preempt blocked filesystem I/O or one bounded
JSON decode. Accounted semantic weights and byte-buffer capacity do not bound RSS.
Provider-private media and wire resources retain their independent limits.

Parked cursors and old service generations continue to consume credit until
their last owner releases it. Pressure can therefore refuse a new projection;
optional compaction and maintenance decline that work, while forced work exposes
a typed capacity failure. Eviction or replay would require a separate policy.

Cancellation, proof publication after read commit, worker-held checkpoint credit,
shared parked-session pressure, immutable request copies, and byte-buffer slices
have isolated behavior tests. The checkpoint wire digest and allocation golden
remain unchanged. Incremental validation and fairness for a valid active scan
remain separate work: an owned reusable scan has no total deadline or timeslice.
