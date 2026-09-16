---
name: Prepared Store reads and independent session writeback
---

## Problem

A cold SQLite validation can occupy the Kernel's entire payload reservation
before materializing any payload. Sequential writeback also makes unrelated
Sessions wait for a cold validation even though SQLite has separate validation
and writer lanes. A cache hint alone cannot guarantee that a validated Session
stays warm between preparation and payload admission.

## Decision

The [Store protocol](../../../../crates/rsi-agent/store-protocol/README.md) owns an
opaque validation lease and scalar watermarks. SQLite keeps actual successful
proofs separate from its eviction policy and shares bounded in-use pins by
Session identity. A pin owns the Store lifetime; a cache proof does not, avoiding
an owner cycle. Pin waiters retain their local successful proof without holding
the validation lane. Drop releases capacity before waking waiters and never
reenters the registry mutex.

Pin insertion owns bounded dead-entry cleanup so ordinary reads do not scan
unrelated leases. The path encoding owns its byte bound beside `AgentPath`;
representation changes must satisfy that protocol's serialization tests rather
than silently disagreeing with arithmetic in a Store adapter.

The [Kernel](../../../../crates/rsi-agent/kernel/README.md) prepares history before
payload admission and dispatches reads with owned guards. Guards also accompany
the task result until delivery. Ordinary caller cancellation cannot release a
reservation while the Store worker still owns its materialized page. Metadata
watermarks require neither replay nor a payload reservation. Atomic writes keep
their existing validation and compare-and-set checks.

Writeback maintains independently polled suffixes, at most one for each resident
Session. Completion immediately makes that Session's next suffix eligible.
The existing resident count and pending byte limits bound concurrency and memory.

## Alternatives considered

Increasing the 64 MiB read budget does not remove the dependency. A fixed small
flush pool can fill with cold Sessions and block warm work. An unbounded queue
would abandon the existing process budget. Multiple SQLite writers add contention
without isolating validation. A bare boolean cache token permits revalidation
inside payload admission after eviction; retaining a successful proof closes
that gap. Making metadata reads validate history recreates the cold dependency.

## Consequences

Pinned proofs add a separately bounded lifetime alongside the eviction cache.
Waiting callers can wait for a distinct-Session pin slot; same-Session callers
share a slot. Indexed strings are checked as borrowed SQLite values before
ownership, while canonical content and relational validation remain independent.

Deterministic tests pause actual SQLite validation while warm Kernel reads,
terminal work and new Sessions progress. Additional tests cover pin saturation,
cache churn, cancellation, zero-decode watermarks and preallocation corruption
rejection. Abrupt Tokio runtime destruction is distinct from caller cancellation;
normal shutdown drains accepted work under the existing failure policy.
