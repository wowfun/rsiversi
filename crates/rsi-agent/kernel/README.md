# rsi-agent-kernel

Durable turn scheduler and write-behind ordinary plugin. The Kernel is the sole
owner of live session state, Fact sequencing, cancellation classification,
executor claims, 200 ms batching, flush retry, and startup interruption repair.
It hard-requires the mechanical `rsi.agent.store` and standing
`rsi.agent.composition` Local contracts and publishes the application and
executor Turn contracts atomically. Every resident session owns one immutable
composition pin. Fresh admission consumes the draft's exact pin. Resume
preparation issues a move-only, Kernel-bound token containing the authoritative
Header and either the existing resident pin or the current cold generation;
only submission of that token can hydrate the session. Resolution therefore
precedes application Workspace mutation, resident capacity, and Fact-log
reads. A resident session keeps its old generation while new or cold sessions
use the current healthy source generation. The executor-facing claim seam
returns that resident pin only after validating the exact claim lease.

The live scheduler is a bounded working set, not a mirror of durable history.
Recovery streams lexical pages of sessions selected by the Store's open-turn
index, retains only nonterminal control, repairs it, and releases the idle
session. Closed historical sessions are not visited. Runtime terminal commits prune turn
control and evict idle sessions; historical queries page through the Store on
demand. The periodic worker rebases its next 200 ms deadline after every scan;
slow Store I/O never causes back-to-back catch-up ticks. Permanent flush
failure is sticky on the session and terminates both explicit durability waits
and attached observations with `TurnError::Flush`; later submissions to that
session receive the same failure. Store diagnostics crossing the Turn seam are
UTF-8-safe and bounded to the durable Agent diagnostic limit. Recovery terminalizes durably cancelled work
as `Cancelled` and all other unfinished work as `Interrupted`. Effect start
Facts require their matching intent to have crossed the durable watermark.

`KernelLimits` owns three process-wide admissions independently of per-session
bounds: total speculative Fact bytes, conservative maximum-page Store-read
materialization, and attached observers. Defaults are 64 MiB, 64 MiB, and
1,024 respectively and configuration may only tighten them. Because the Store
read contract bounds a page at 64 MiB but does not accept a caller byte limit,
a tightened read budget admits one maximum-sized Fact per page; multi-Fact
reads require the complete page bound. A budget below the maximum checkpoint
blob bound disables checkpoint reads, maintenance rebuilds, and writes as one
feature rather than rebuilding a cache that the Kernel cannot later admit.
Resident capacity
counts both installed sessions and distinct in-flight hydration leaders before
Store I/O; followers for the same session share the leader's reservation.
Fresh-session reservations use that same capacity before checking durable
identity, so fresh/resume races cannot overbook the process through header I/O.
Both fresh reservations and hydration leadership are cancellation-safe: owner
drop releases the exact capacity reservation, and a cancelled hydration leader
settles all followers before removing the shared load. Resume submissions pin
their hydrated session through admission; once the last pin is released, a
failed admission immediately evicts an otherwise idle session. Fact publication
reserves aggregate process bytes before installing any turn control or pending
Fact, so capacity rejection leaves no partial live state. Observation consumes
watch watermarks with `borrow_and_update`, preserving durability advances that
arrive while the stream is not being polled. Durable observation reads bounded
pages and emits their Facts incrementally; speculative lookup is direct within
the contiguous pending suffix. Flush selection snapshots only `Arc` Fact
handles while holding the global Kernel lock; materializing the Store-owned
batch occurs after that lock is released.

Shutdown closes admission before its final flush, settles any in-flight cold
hydration as shutting down, and releases all resident sessions after the worker
has joined. Escaped service handles therefore cannot keep composition
generation pins or speculative Kernel state alive after shutdown returns.

The Kernel stores Context checkpoints only after verifying that the claimed
turn is durably terminal and that the checkpoint covers the unchanged durable
and live tail. The terminal claim retains an opaque Kernel-issued binding over
all of its public fields, so maintenance can reject fabricated or mutated
claims after normal live claim ownership has retired. A maintenance-only unfiltered read can include later accepted
turns in the Context-owned checkpoint; each later claim carries its own exact
acceptance sequence so the executor restores only a checkpoint preceding that
claim. Store CAS and Context's exact-prefix proof reject
concurrent tails; a cache failure does not affect the canonical Fact log.
Startup recovery revalidates accumulated usage and any exhaustion marker
against the immutable session budget before choosing a repair outcome. Repair
uses only durable Header/Facts and does not build an Agent composition; the
current generation is acquired later by cold resume before resident admission.
