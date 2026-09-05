---
name: Durable session kernel over ordinary plugins
comment: Separate durable session ownership from live executor generations
---

## Problem

This note owns the generic durable Kernel, claim, recovery, and checkpoint
mechanics. The user-facing tree/fork policy and owner-declared Tool scheduling
have narrower owners in
[Recoverable subagent trees](../feature/2026-09-03-recoverable-subagent-tree.md)
and [bounded Tool overlap](2026-09-03-owner-declared-tool-overlap.md).

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

One executor generation may hold multiple concurrent Kernel claims through a
validated `maximum_active_turns` pool. Kernel scheduling stays authoritative:
its durable ready indexes choose claimable work, its per-Session gate prevents
two active turns in one Session, and terminal durability gates the next claim.
Within an Agent tree, waking messages are ordered by acceptance timestamp,
Session identity, and control sequence. The bounded root scan skips a tree that
already has three running Turns, leaving one lane in the standard four-lane
product for another Session. A parked activation remains durable but holds no
executor lane and counts only against the 256-node durable tree bound. A path
depth of three means three child edges below the root. The executor adds no
ready queue or per-Session scheduler. Its activation-level coordinator owns all
lanes, one shared shutdown deadline, and retained-effect cleanup after every
lane settles. Waiting for Kernel work does not reserve execution admission. At
most the coordinator's next claim waits for a lane, and shutdown releases that
claim. Ending a lane closes its parking authority and reclaims admission even
if retained Tool work still owns typed execution extensions.
Ready-root selection carries a rotating cursor across bounded pages. One root's
corrupt or temporarily failed Store scan is isolated from later roots and the
executor lease, and an idle scheduler retries on a bounded fallback tick. The
tree-capacity check and following atomic claim remain one serialized local
decision because the Store does not yet expose a combined tree-lane reservation.

The Kernel owns an in-memory speculative Fact suffix after the Store's durable
prefix. A direct-Turn receipt is returned only after its accepted Fact is
durable. Product Language and multimodal submission instead atomically persists
a caller-owned `MessageId` and, for a new Session, its Header in the Agent
control stream. That durable receipt has no speculative `TurnId`; claim later
creates the Turn, Step, and model-visible input in one Store transaction. The
receipt carries both its acceptance control cursor and the Fact tail observed
by the same mailbox snapshot, so a reconnecting client can subscribe for that
later claim without replaying old Facts or polling message status. Its
worker scans eligible contiguous batches at least every 200 ms, with the Store-owned
limits of 512 Facts and 64 MiB encoded bytes; Store admission and I/O determine
commit latency. Transient append failure retains the exact suffix and pauses
new external effects until a bounded retry sequence commits it or latches a
permanent flush failure. The latch rejects later submissions to the affected
session rather than accepting work that can never be claimed. Submission
admission also closes the controls-only CAS window left by a write-behind append
that was already in flight: the Kernel waits for that exact resident suffix,
refreshes the Fact-less append cursor, and retries the atomic commit once.
Fact-bearing or nonresident conflicts are not reclassified. A direct-Turn
submit result is the caller's Turn identity plus its exact durable acceptance
sequence; observation is opened separately.

Agent-to-Agent delivery does not branch on an unlocked observation of target
state. `send_message` always targets the next Step and remains held while the
target is idle. `followup_task` always queues a waking next Turn, including
behind an already-running Turn. Completion delivery is Kernel-owned and selects
next Step versus next Turn while holding the relevant atomic Store guards.

Every external effect follows prepare, durable intent, durable start, invoke,
and durable outcome order. The Kernel accepts a start marker only after its
matching intent is durable. Cancellation first appends and durably commits its
`CancelRequested` Fact, then fires the live cancellation token independently
of whether the requesting future remains attached. A following
terminal outcome is classified by the already-recorded cancellation and
remains single-assignment. A `Cancelled` proposal without that durable Fact is
canonicalized to a bounded failure at the Kernel boundary. The next turn in a session is not claimable until
the previous terminal Fact is durable.

Startup recovery handles accepted Turns and unclaimed Messages as distinct
durable states. It enumerates the bounded open-turn index, reads and validates
each selected per-turn Fact stream, then appends a `Cancelled` terminal when the
prefix contains a durable cancellation and a deterministic `Interrupted`
terminal for every other accepted nonterminal Turn. Accepted Turns and
unknown-start external effects are never replayed. Unclaimed waking Messages
instead remain in the bounded mailbox and ready indexes: the executor scans a
bounded root page after restart and claim atomically creates their Activation,
Turn, Step, and model-visible input. Recovery does not materialize every mailbox
or historical Agent tree into resident scheduler state. Mailbox reads return the
complete pending count but only an ordered 32 MiB payload prefix and one selected
status from a single Store snapshot; Kernel holds its process-wide Store-read
reservation through decode and validation. Capacity and completion decisions use
a separate metadata-only Store snapshot containing the exact pending count and
Fact/control tails, so those paths do not decode the payload prefix. Scheduler,
cancellation, and settlement paths that need only descendant identities reuse
the Store's bounded recursive descendant snapshot; presentation-oriented tree
listing still reads the parent, path, and task metadata it returns.
Activation terminal preparation may perform descendant work before it closes
the durable Turn. It therefore acquires the child and optional parent submission
admissions before reading and flushing the final live tail. A concurrently
accepted Turn is included in that prefix; it cannot turn an ordinary race into
an internal invariant failure between an earlier flush and the terminal commit.
The executor reconstructs a fresh `ContextFold` from count-bounded Store pages
for each claimed turn and incrementally synchronizes it while that turn runs.
Context projection removes only complete oldest turns under its message and
byte bounds; the durable Fact log remains complete.

## Alternatives considered

Returning a live-only admission from submit was rejected for the product-wide
Session application because reconnectable idempotency requires an unambiguous
durable message receipt. Allocating a Turn at mailbox acceptance was rejected:
accepted input may wait without consuming live Turn state, while a claim can
atomically bind the exact activation and Step. Internal Fact publication remains
write-behind; executor effects still use durable watermarks and explicit flush
operations, and a terminal outcome is not published before its prefix is
durable.

Treating parked Agent supervision as an executor lane was rejected because a
tree waiting on descendants would reduce useful global progress. Letting one
tree run all four standard lanes was also rejected because an independent
Session then has no progress slot. Parked activations remain bounded durable
state, while the separate three-running-Turn tree limit preserves that slot.

Store-owned effect transitions and recovery classification were rejected
because they make persistence adapters semantic and shallow. A mechanical
acceptance/terminal index is retained because the alternative makes every cold
query and restart decode unrelated lifetime history. Executor Fiber lifetime
was rejected as session lifetime because plugin replacement is not a durability
event.

A per-session LRU fold cache, a replace-on-write control snapshot, and a separate
session blob service were not adopted. They
need measured pressure and their own invalidation or durable-wire contracts;
none is part of the implemented foundation.

## Consequences

A process crash can discard a speculative direct-Turn suffix only before submit
returns its receipt. Mailbox acceptance is already a durable control commit when
its receipt returns. Retrying a caller-owned Message or direct-Turn identity
resolves against the appropriate durable boundary and cannot re-execute a
different canonical body under the same identity.

Executor context reconstruction first restores an integrity-checked Context
checkpoint when its immutable header, retention limits, cursor, and durable
Fact-prefix digest still match, then reads only the remaining suffix. A missing
or invalid checkpoint falls back to bounded reads of the durable session
prefix. Terminal and idle session control is evicted and loaded on demand
through the turn/open-turn indexes; active state remains bounded by the
session, checkpoint, per-read, projection, pending-suffix, and Store limits.
Checkpointing is a cache optimization and does not change durable Fact truth or
make projection constant with session length.
For a forked Session's first checkpoint, a separate maintenance reader
authorizes the immutable child Header after terminal publication and folds the
selected parent prefix before the child's own Facts; checkpoint construction no
longer depends on the live-turn fork reader. A missing, stale, corrupt, or
unavailable checkpoint remains a cache miss and falls back to canonical Facts.
The optional checkpoint writer coalesces the latest request per Session while
retaining FIFO across Sessions. Capacity is bounded by distinct pending Session
keys; saturation may discard a new cache request but never evicts durable Facts
or changes claim ordering. Executor shutdown stops and joins the claim lanes
before closing checkpoint admission, then drains already accepted cache work
within the same absolute shutdown deadline.

SQLite reopen validates root ownership and its exact schema. First session
access validates that session's bounded Header, relational integrity, and
durable watermark; an explicit offline verifier performs the complete physical
and logical database audit. Kernel recovery revalidates typed headers and Fact sequences, while
CAS digest and length are verified on exact read; startup deliberately avoids a
full scan of every referenced immutable object. Durable session meaning
survives executor, AI, and Tool generation replacement because only the
mechanical Store and Kernel own it.
