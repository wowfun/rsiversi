---
name: Agent metadata snapshots and ownership of admitted work
comment: Keep durable commits independent of cancelled waiters and unrelated roots
---

## Problem

Metadata access triggered mechanical history scans. A short-lived async waiter
could release source authority or Store-root ownership while a dispatched
operation was still committing. Runtime ancestor settlement shared the only
flush worker, and ready selection held a global mutex across composition and
workspace preparation. These lifetimes allowed unrelated work to stall and made
cancellation an unreliable boundary for claim retirement.

## Decision

The [Store contract](../../../../crates/rsi-agent/store-protocol/README.md)
separates bounded immutable Header reads from explicit session validation. This
supersedes the metadata-first-access part of the
[earlier validation decision](2026-09-01-agent-store-validation-and-read-snapshots.md).
Execution, recovery and control decisions keep the mechanical proof; metadata
reads neither fill nor refresh its bounded cache. One subtree snapshot supplies
lineage and activity, with mechanical proofs for uncached descendants in that
same snapshot. Quiescence checks every strict descendant after applying all
appends, before committing; newly obtained proofs are cached only after commit.

The [Serve application history contract](../../../../apps/serve/README.md)
uses the caller-authorized [Session summary](../../../../crates/rsi/session-protocol/README.md)
for metadata-only requests. Its Header, activity time and Fact/control watermarks
come from one Store metadata snapshot. SQLite already decodes those fields
together; discarding the mutable fields and querying them again adds a reader
job and combines independent cuts. Header-only Agent Store callers retain their
focused interface through the same concrete row reader, without another boxed
Store call. Attaching a Session merely to return its stored cursor would register
runtime activity and allocate handle services and watch state. DSH's JSONL persistence separates
`stat()` from `open()` for the same reason: observation does not need a live log
handle. RSI keeps its location authorization, direct-parent check and request
retirement boundary, without adding another metadata cache or durable index.

The [Kernel contract](../../../../crates/rsi-agent/kernel/README.md) gives
accepted Agent mutations an owned commit task and a move-only source lease.
Source release closes new admission and defers claim retirement until accepted
work returns and installs its durable result. Terminal work drains source leases
before taking submission locks. Activation terminals cannot bypass their atomic
settlement through raw Fact publication. Before terminal admission, a rejected
or abandoned preparation restores the same live claim's mutation admission only
after its last accepted mutation drains. Restoration installs a new stop token
and never revives a retired, cancelled, replaced, or terminal claim. An owned
terminal commit retains closure through acknowledgement uncertainty; retrying
terminal settlement remains available. Exact spawn retries recover the initial
receipt because cancelling a waiter cannot undo an admitted child creation.
Flush and settlement use independent workers;
ready preparation uses bounded per-root reservations outside the scheduler lock.
SQLite ready-root enumeration must also bound the work behind one scheduler page:
it seeks each distinct root through the index under one read snapshot, so
duplicate ready messages do not determine the work needed to fill a root page.
This is scheduler progress work, independent of Session export or GUI previews;
the [SQLite contract](../../../../crates/rsi-agent/store-sqlite/README.md) and
selection-work tests own its mechanics and measured scan bounds.
New durable input and newly available tree capacity request a root rescan even
when another root's final-page preparation is blocked; failed enumeration still
keeps its retry deadline.
Shutdown closes producers before draining admitted tasks with flush still alive.

The [SQLite owner](../../../../crates/rsi-agent/store-sqlite/README.md) retains
connections, CAS work and the persistent writer lease as one allocation. Last
owner destruction closes the connections before explicitly unlocking the lock
file. Schema checks compare the exact DDL emitted at creation.

First Session validation proves mailbox, ready and activation indexes against
canonical controls before cached control reads or transaction guards use them.
Replay releases completed message payloads immediately and retains the bounded
pending set. Scheduler failure diagnostics include transient ready enumeration
failures even when retry later succeeds without losing executor registration.

Ready selection reads the mailbox's existing source discriminator through a
bounded metadata join. Ordinary candidates do not acquire Session submission
admission or decode pending payloads merely to reject a busy Session. Automatic
input still uses the existing admitted cleanup and durable watermark checks.
Warm bounded fork selection counts only its selected suffix; interval and
digest validation retain their own work. Waiting enumeration has a predicate
index in the exact Store schema. These choices reduce unnecessary work without
adding a mutable mailbox cache or another scheduling owner.

## Alternatives considered

Revalidating history for every metadata row repeats work without increasing the
proof required for presentation. A validation no-op would instead remove a
required execution boundary. Keeping both old and new query APIs would obscure
which path establishes that proof.
Caching an earlier summary would not provide fresh watermarks under current
caller authority. Parallel independent metadata reads still perform both jobs
and cannot establish one snapshot. The summary operation replaces the Header-only
Session operation without a pre-release compatibility alias.
Cache hints are optional: a poisoned hint cache disables reuse instead of
overriding a durable commit result. Its failure cannot authorize skipping the
uncached proof.

A source submission mutex held across target preparation can deadlock reciprocal
operations and terminal publication. Rechecking a source only before the last
await leaves a race at actual mutation. Aborting an admitted task cannot undo a
SQLite or filesystem operation already running on another thread. SQL text
normalization is not a semantic schema proof because it changes string literals.

## Consequences

Point-read replies carry three numeric metadata fields and reuse the recent-listing
summary type. They add no durable field, mirror, background task or cache. Removing
one reader job does not by itself establish end-to-end search performance parity.

Historical corruption can coexist with readable immutable metadata. Explicit
validation, indexed execution/history reads and offline verification still reject
it. The later [canonical page decision](2026-11-08-canonical-pages-and-execution-proofs.md)
separates raw history intervals from these whole-Session proofs.
A caller may stop waiting before learning that an admitted mutation committed.
Shutdown can report its bounded timeout while background drain continues to own
its resources. Runtime ancestor settlement is eventually observable independently
of the successfully committed child terminal. Fixed capacities remain policy,
with deterministic barrier tests proving ownership and bounds; latency is
reported separately and is not a correctness threshold.
