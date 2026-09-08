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

Agent mutations prepare and acquire target admission before their final source
check. That check, under the short Kernel state lock, verifies the exact claim,
executor registration, open mutation gate, and execution cancellation. An
accepted mutation holds a move-only source lease through its owned commit task,
Store retries, and resident installation even when the caller stops waiting.
Spawn retries serialize on the child identity and return the existing initial
message receipt only when the parent, invoking Turn, fork selection, task, Header
and message match. Cancellation stops the waiter; an admitted creation remains
discoverable through that exact retry or ordinary Agent listing.
Release or executor withdrawal closes that gate and defers claim retirement
until the last accepted mutation completes, then clears its owner and requeues
only nonterminal Turns. Terminal publication closes and drains the same gate
before acquiring any submission admission. Drain uses the
one-minute durability deadline and never forcibly releases accepted ownership.
Before terminal admission, a rejected or abandoned attempt restores mutation
admission after the last retained mutation drains, provided that the same claim
is still live and has not been retired. Restoration installs a fresh stop token;
it cannot revive a cancelled, replaced, or terminal claim. Once the terminal
commit task owns admission, its gate remains closed through Store reconciliation.
One mutex owns the gate's admission phase (`Open`, `Closed`, `ReopenPending`,
or `TerminalAdmitted`), retirement flag, terminal drainer, retained wait, and
active lease count. Retirement and draining are independent dimensions. Lock
order is Kernel state then gate state; a gate guard never survives an await or
an attempt to acquire Kernel state. The last lease release and drainer drop
use the same reopening rule, and an admitted terminal never reopens.
Factory preparation validates limits once and retains a private typed value
alongside normalized configuration; activation consumes that value. Public
recovery constructors still validate raw caller-supplied limits.

Durable observers subscribe to Session changes before their first read.
Successful Store commit watermarks notify only touched Sessions; publication
of a new immutable Header also notifies that root's tree-membership watch.
Watch entries exist only while subscribed, coalesce revisions without payload
queues, and are independent of the scheduler's global claim notification.
Observers mark a revision before collecting a page and keep a five-second
fallback for changes recovered outside their process-local notification path.

Observer retention has its own default 64 MiB canonical-payload budget, separate
from read materialization.
The pool is shared by all Sessions and observers of one Kernel: retained handles
from one consumer can cause `Capacity` in another. Release those handles before
retrying from the last delivered cursor.
A read reserves its page's actual retained bytes before releasing transient read
admission. Retained admission never waits:
failure releases the page and returns `Capacity` without delivering a cursor.
Durable observations retain at most one page, alternating controls and Facts;
live Facts are also charged. The item handle owns the charge through all
consumer clones, including a renderer queue after the stream is detached.
Configuration can tighten the pool only while admitting one maximum Fact.
Activation terminals must use `finish_activation_turn`, which commits the
terminal and activation transition together; raw terminal publication is only
valid for direct Turns.
Agent interruption publishes its cancellation Fact only after the direct atomic
commit succeeds. Ordinary external cancellation retains write-behind behavior.

A parked human or Agent wait retains its mutation lease while its durable resume
is recoverable. One claim can own only one wait through completion of its cleanup;
a second park is rejected before touching durable state or elapsed admission.
Resume checks the exact activation under retained Session admission
and accepts an already-running activation after a lost commit acknowledgement.
An attempted park owns reconciliation before its commit acknowledgement: cleanup
reads back the activation and resumes a committed park, including when the park
acknowledgement was lost. A failed park reports its original cause; if cleanup
also fails, the bounded diagnostic includes both failures.
I/O, cursor contention, and admission waits retry every five seconds independently
of cancellation or shutdown. A deterministic ownership, lifecycle, or Store
validation failure stops retrying, latches a bounded permanent Session failure,
and releases the wait's local ownership. The failed Session cannot be reclaimed
for another model run; durable recovery remains responsible for its unfinished
activation. The resume caller waits at most one minute; timeout or caller drop
cancels lane reacquisition while the tracked cleanup retains ownership until
storage recovers. Failed executor parking uses the same bounded waiter and owned
cleanup. An unavailable or corrupt Store cannot be reported as a completed resume
or a successful drain. Only human waits pause elapsed execution; Agent waits retain the
ordinary execution budget.

Flush and waiting-activation settlement have independent workers. Runtime
settlement retains its cursor across 16-session slices, isolates local errors,
and exposes bounded read-only health; startup recovery remains fail-closed.
Global diagnostics clear only after a complete error-free scan, while an
enumerator failure backs off independently of wake notifications.
Finite admitted tasks share one task tracker. Shutdown closes producers and
admission before draining that tracker with flush still running. The public
shutdown wait has one five-second budget for the whole drain, including worker
joins. Slow admitted work can outlive that budget; background drain retains all
live resources through final flush, worker exit, and resident quiescence.
Parent settlement that becomes eligible after the settlement producer exits
remains a durable waiting activation for the next recovery. Shutdown drains
accepted commits, not every subsequently eligible ancestor transition.

Durable waking-message selection rotates a cursor and caches at most 256 roots
under a short scheduler lock. Page reads and preparation run outside that lock,
with at most four preparation jobs and one job per root. New durable input
requests another root scan even while the previous final page still has a
blocked preparation; retained preparations continue to count against the bound.
A generation-bound reservation releases the exact root on completion. A candidate reserves one of
its tree's three lanes before composition/workspace preparation and transfers
that lane through the resident Turn into the executor claim. Failure in one root's bounded Store scan is
isolated from later roots and from the executor lease; an otherwise idle claim
loop retries skipped roots every five seconds as well as on commit notification.
Enumeration failures retain their cursor but fence further page reads for five
seconds, including reads triggered by unrelated claim notifications.
Transient enumeration I/O is retried. Other global enumeration errors propagate
through `claim` and stop the receiving executor pool; the health snapshot retains
the bounded diagnostic.

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

`KernelLimits` owns four process-wide admissions independently of per-session
bounds: total speculative Fact bytes, conservative maximum-page Store-read
materialization, retained observation bytes, and attached observers. Defaults are
64 MiB, 64 MiB, 64 MiB, and
1,024 respectively and configuration may only tighten them. Because the Store
read contract bounds Fact and control pages at 64 MiB, while the indexed
mailbox has its own 32 MiB pending-prefix bound plus one selected message; the
Kernel reserves those bounds before Store I/O. A tightened read budget admits
one maximum-sized Fact per page; multi-Fact reads require the complete page
bound. A budget below the maximum checkpoint
blob bound disables checkpoint reads, maintenance rebuilds, and writes as one
feature rather than rebuilding a cache that the Kernel cannot later admit.
Indexed turn-boundary and cold Header reads use the same admission. Retry checks
read and compare the small Header before materializing the acceptance, then
consume that acceptance without cloning it across another Store round trip.
Capacity and completion paths use the Store's metadata-only mailbox summary,
so reading an exact count and Fact/control tails does not reserve or decode the
payload prefix. Identity-only Agent-tree scheduling and settlement reuse the
Store's one-call recursive descendant snapshot.
Tree selection for control and approval operations validates the requested
Session before using its Header lineage, even when presentation can read its
bounded metadata independently.
Waiting activation settlement is retried by the Kernel worker after an
in-process settlement failure and by a bounded five-second fallback scan, so a
durable descendant terminal does not require process restart to settle its
ancestors.
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
Activation terminal preparation acquires the child and optional parent
submission admissions before taking and flushing its final live-tail snapshot;
a Turn accepted during earlier descendant preparation therefore becomes part of
that terminal's durable prefix instead of producing a contradictory-state error.
A write-behind append may already be in flight when submission admission is
acquired. A controls-only Agent commit that loses its Fact-tail compare to that
exact resident suffix waits for the suffix to become durable, refreshes only
the affected Fact-less append cursor, and retries once. Other Store conflicts
retain their ordinary failure meaning.
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
pages and emits their Facts incrementally. Before retention or cursor advance,
both Fact and control pages must echo the requested cursor, respect the requested
count, and pass their bounded contiguous-page validation. A mismatched response
ends observation without delivering any of that page.
In-process commits wake only observers of the affected Session; a bounded
five-second poll also reconciles missed commit acknowledgements. Tree-membership
watches follow this Kernel's root and child creation notifications, including
atomic and write-behind creation attempts with an uncertain Store acknowledgement. Such a notification
is a requery hint, never proof of commit. The standard
Host owns the sole Kernel and Store writer lease; concurrent external writes
are outside that ownership contract. Speculative lookup
is direct within the contiguous pending suffix. Flush selection snapshots only
`Arc` Fact handles while holding the global Kernel lock; materializing the
Store-owned batch occurs after that lock is released.

Successful shutdown releases resident sessions and generation pins after all
admitted tasks and workers have joined. A bounded timeout reports that drain
continues; escaped handles cannot admit new work, and background completion
releases pins only after actual admitted work finishes.

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
