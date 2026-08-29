---
name: Durable session kernel over ordinary plugins
comment: Separate durable session ownership from live executor generations
---

## Problem

Agent sessions must retain accepted turns, effect evidence, terminal outcomes,
and recovery meaning across executor replacement and process restart. An
executor-owned session would change lifetime with its plugin generation, while
a Store-owned state machine would duplicate Agent semantics across persistence
adapters.

## Decision

`SessionKernel` owns session headers, Fact sequencing, sequential turn
admission, executor claims, cancellation classification, live observation,
write-behind, terminal single-assignment, finalizer ordering, and startup
recovery. `SessionStore` remains a mechanical exact-schema
compare-and-append/read seam with Memory and SQLite/filesystem-CAS adapters.
It transactionally indexes exact turn membership and terminal presence so the
Kernel can read a cold turn or enumerate open turns without scanning unrelated
Facts. It does not apply effect transitions, classify recovery, or choose an
outcome.

The SQLite Store, Kernel, and executor are independent ordinary Meta plugins.
The Kernel requires the Store Local contract and publishes the process-local
Turn application, execution, and finalization contracts. The executor requires
those Turn contracts plus the exact Language, Image, Media, and Tool Local
services it uses. There is no family adapter or public
`rsi.agent.sessions` service.

The Kernel publishes an accepted Fact to the live stream immediately and owns
an in-memory speculative suffix after the Store's durable prefix. Its worker
scans eligible contiguous batches at least every 200 ms, with the Store-owned
limits of 512 Facts and 64 MiB encoded bytes; Store admission and I/O determine
commit latency. Transient append failure retains the exact suffix and pauses
new external effects until a bounded retry sequence commits it or latches a
permanent flush failure. The latch rejects later submissions to the affected
session rather than accepting work that can never be claimed. A submit result
is a turn identity, not a durability receipt; observation is opened separately.

Every external effect follows prepare, durable intent, durable start, invoke,
and durable outcome order. The Kernel accepts a start marker only after its
matching intent is durable. Cancellation first appends and durably commits its
`CancelRequested` Fact, then fires the live cancellation token independently
of whether the requesting future remains attached. A following
terminal outcome is classified by the already-recorded cancellation and
remains single-assignment. A `Cancelled` proposal without that durable Fact is
canonicalized to a bounded failure at the Kernel boundary. The next turn in a session is not claimable until
the previous terminal Fact is durable.

Startup recovery enumerates the bounded open-turn index, reads and validates
each selected per-turn Fact stream, then appends a
`Cancelled` terminal when the prefix contains a durable cancellation and a
deterministic `Interrupted` terminal for every other accepted nonterminal turn.
It does not replay even known-not-started work: doing so for unbounded durable
history would violate the Kernel's bounded resident-session contract without a
separate durable queue and admission policy. Unknown-start external effects are
never replayed.
The executor reconstructs a fresh `ContextFold` from count-bounded Store pages
for each claimed turn and incrementally synchronizes it while that turn runs.
Context projection removes only complete oldest turns under its message and
byte bounds; the durable Fact log remains complete.

## Alternatives considered

Synchronous durability before returning from every submit was rejected for the
chosen low-latency live admission contract. Callers that need an effect fence
use the durable watermarks and explicit flush operations; terminal outcome is
not published before its prefix is durable.

Store-owned effect transitions and recovery classification were rejected
because they make persistence adapters semantic and shallow. A mechanical
acceptance/terminal index is retained because the alternative makes every cold
query and restart decode unrelated lifetime history. Executor Fiber lifetime
was rejected as session lifetime because plugin replacement is not a durability
event.

A per-session LRU fold cache, byte-budgeted Store pages, a replace-on-write
control snapshot, and a separate session blob service were not adopted. They
need measured pressure and their own invalidation or durable-wire contracts;
none is part of the implemented foundation.

## Consequences

A process crash before the first write-behind flush can lose a newly accepted
live turn. Live observations and durable watermarks expose that interval
instead of claiming it is durable.

Executor context reconstruction currently rereads the durable session prefix
for each claimed turn. Terminal and idle session control is evicted and loaded
on demand through the turn/open-turn indexes; active state remains bounded by
the session, per-read, projection, pending-suffix, and Store limits. Context
projection is still not claimed to be constant with session length.

SQLite reopen validates its exact schema, relational integrity, and durable
watermarks. Kernel recovery revalidates typed headers and Fact sequences, while
CAS digest and length are verified on exact read; startup deliberately avoids a
full scan of every referenced immutable object. Durable session meaning
survives executor, AI, and Tool generation replacement because only the
mechanical Store and Kernel own it.
