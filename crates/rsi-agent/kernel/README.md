# rsi-agent-kernel

Durable turn scheduler and write-behind ordinary plugin. The Kernel is the sole
owner of live session state, Fact sequencing, cancellation classification,
executor claims, 200 ms batching, flush retry, and startup interruption repair.
Submission returns the caller-owned turn identity and exact acceptance sequence
only after that acceptance is durable; its bounded durability wait reports a
flush failure rather than waiting forever when the worker or Store cannot make
progress.
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
returns that resident pin only after validating the issuer seal, live claim
identity, and pointer identity of the one resident Header allocation.

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
UTF-8-safe and bounded to the durable Agent diagnostic limit. Ordinary Store
access failures use `TurnError::Store`; they are not mislabeled as a durable
flush failure. Recovery terminalizes durably cancelled work
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
Indexed turn-boundary and cold Header reads use the same admission. Retry checks
read and compare the small Header before materializing the acceptance, then
consume that acceptance without cloning it across another Store round trip.
Resident capacity
counts both installed sessions and distinct in-flight hydration leaders before
Store I/O; followers for the same session share the leader's reservation.
Submission admission is likewise keyed by Session identity: retries for one
Session serialize across their Store checks, while independent Sessions may
progress concurrently under a process-wide bound of 256 admission slots. Slot
capacity is acquired only by the current owner of a Session key, so queued
same-Session retries do not consume unrelated active slots. Slot and
same-Session admission waits share the one-minute durability deadline, and
shutdown closes both wait paths before the final flush.
Fresh-session reservations use that same capacity before checking durable
identity, so fresh/resume races cannot overbook the process through header I/O.
Both fresh reservations and hydration leadership are cancellation-safe: owner
drop releases the exact capacity reservation, and a cancelled hydration leader
settles all followers before removing the shared load. Resume submissions pin
their hydrated session through admission; once the last pin is released, a
failed admission immediately evicts an otherwise idle session. Fact publication
consumes owned bodies and reserves aggregate process bytes before installing
any turn control or pending Fact. Closing Kernel admission fences publication
before it can install live or pending state. A candidate batch that cannot fit
even in an empty per-Session or configured process budget is invalid; capacity that this
Session can release returns the canonical unpublished bodies as
`FlushRequired`; transient process capacity held by another Session instead
waits, within the durability bound, for a global commit notification. A Session
that latches a permanent flush failure also wakes its own publication waiting
on that global capacity signal, so the publication observes `TurnError::Flush`
instead of expiring as generic capacity pressure.
Only a successful commit creates shared Fact handles, so a retry neither clones
payloads nor leaves partial live state.
Observation consumes
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
and live tail. The terminal claim retains private immutable fields and its
Kernel issuer seal, so maintenance can reject foreign claims after normal live
claim ownership has retired. A maintenance-only unfiltered read can include later accepted
turns in the Context-owned checkpoint; each later claim carries its own exact
acceptance sequence so the executor restores only a checkpoint preceding that
claim. Store CAS and Context's exact-prefix proof reject
concurrent tails; a cache failure does not affect the canonical Fact log.
Startup recovery revalidates accumulated usage and any exhaustion marker
against the immutable session budget before choosing a repair outcome. Repair
uses only durable Header/Facts and does not build an Agent composition; the
current generation is acquired later by cold resume before resident admission.
