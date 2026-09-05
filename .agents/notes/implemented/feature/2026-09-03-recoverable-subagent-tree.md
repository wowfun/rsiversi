---
name: Recoverable subagent trees with bounded canonical forks
comment: Give every child a durable identity while keeping lifecycle and authority in the Agent plane
---

## Problem

This feature note owns the user-visible subagent tree, delivery, supervision,
and fork policy. Generic durable claim/recovery/checkpoint mechanics remain in
the [Session Kernel note](../architecture/2026-08-26-durable-session-kernel.md),
while current interfaces are documented in the
[Agent architecture](../../../../crates/rsi-agent/docs/architecture.md) and
[Agent Tools package](../../../../crates/rsi-agent/tools/README.md).

A subagent must remain observable, controllable, and continuable after executor
replacement or process restart. Treating it as an in-memory task or child Fiber
would bind its identity and history to one live generation. Provider-private
replay state adds another boundary: an extension format is not proof that two
calls share one endpoint, configuration generation, and credential source.

Parent completion introduces a separate lifecycle problem. A Turn can finish
while descendant work is still live, and a cold child must be able to publish its
completion even when the parent mailbox has otherwise reached capacity. Model
arguments also cannot be trusted to name the calling Session, Turn, or tree.

## Decision

Every spawned child is a new durable, continuable Session. Its immutable Header
records the exact parent and root Sessions, bounded tree path, sibling-unique task
name, invoking Turn, parent Header fingerprint, requested fork selection, resolved
balanced Fact interval, and terminal-prefix digest. The standard tree permits at
most three child edges below the root, 256 durable Sessions, and three running
Turns in one Kernel process. The exclusively owned standard Session Host keeps
that scheduler singular. The fourth standard executor lane therefore remains
available to an independent tree; a parked activation holds no lane.

`fork_turns` accepts `none`, `all`, or a positive completed-Turn count. Resolution
ends strictly before the invoking Turn and retains only a balanced terminal
prefix. Per-Turn terminal-prefix digests make selection and integrity checking
independent of a sequence-one hash walk. Context folds the selected canonical
parent Facts into the child. Durable Facts retain provider response evidence for
audit, but a later request preserves only the complete visible provider-neutral
prefix: it neither elides that prefix with a replay token nor forwards
provider-private reasoning. The child inherits the parent's frozen route,
policy, workspace, trust decision, and preset identity without mutating or
truncating the parent.

The model-facing interface contains exactly `spawn_agent`, `send_message`,
`followup_task`, `wait_agent`, `interrupt_agent`, and `list_agents`. These Tools
are thin adapters over `TurnService`. The executor derives an
`AgentCallerAuthority` from the sealed live claim and injects it through the
typed Tool execution-extension seam; no model argument carries a root identity,
claim seal, or authority token. Kernel lineage rules authorize only the intended
adjacent, ancestor, descendant, or whole-tree operation.

Delivery horizons are fixed by the requested operation. `send_message` targets
the next Step and stays held while the target is idle; `followup_task` always
queues a waking next Turn. A completion message enters the activation-owned running parent's next
Step or wakes an idle parent, chosen atomically by the Kernel. Any next-Step
completion still pending at the parent's activation terminal is promoted by a
durable control in the same settlement transaction, so it cannot remain
stranded after settlement. Ordinary fixed-horizon next-Step messages stay held.
`wait_agent`
durably parks supervision and releases its executor lane until next-Step input
in its own mailbox, a descendant control change, a message,
completion, timeout, cancellation, or interruption resumes it. Before parking,
the Store captures descendant membership and all control watermarks in one
snapshot and Kernel revalidates the visible tree. Each later observation
reissues that same bounded atomic snapshot rather than querying every descendant
and rebuilding the tree; only a changed descendant needs bounded, paginated
control reads through its exact changed interval to classify message versus
completion. If no descendant is live, the call performs one revalidated check
and returns without inventing park/resume evidence. In-process commits wake the wait
directly; a five-second Store check is only the fallback for external writers,
and one absolute caller deadline bounds every check.
The target/`wake_required` tuple is exact: only `NextTurn` wakes scheduling.
Every mailbox mutation and direct activation commit for one Session shares the
same admission, so speculative Fact publication cannot cross a durable control
commit. Activation terminal installation requeues the next oldest Turn and
releases an otherwise idle resident Session.

Parking is itself recoverable state. If shutdown or a crash leaves an Activation
parked, recovery first records a cancel-caused resume and only then transitions
the already-terminal Turn to descendant waiting. The Store therefore never has
to reinterpret a parked row as running or accept an impossible phase jump.
Each live claim owns one of three per-root Kernel permits, including direct
Turns. Durable parking releases that permit; resume reacquires it before
publishing a successful resume. The permit is shared only by staged copies of
the same claim and expires with its final owner. Idle roots retain no live
semaphore; later claims discard expired weak entries. Executor-lane reacquisition observes the Turn cancellation token; cancellation
cannot leave a parked Tool waiting indefinitely for a permit, and a successful
wait result is returned only after admission is reacquired.
The same recursive snapshot supplies identity-only membership to ready-tree
validation, descendant cancellation, quiescence settlement, and spawn capacity.
Only `list_agents` and ordered tree presentation rebuild rich child descriptors,
because they also require parent identity, path, and task name.

A Turn terminal and Activation settlement are distinct. Settlement requires
descendant quiescence. Failure, cancellation, or budget exhaustion durably
requests cancellation of every open descendant Turn, preserves already accepted
descendant inbox messages, releases the parent's lane, and leaves the Activation
waiting until the tree is quiet. Each child reserves parent mailbox capacity when
its Activation is claimed, so a cold continuation cannot become unable to settle
after it has started. The settlement transaction removes that reservation with
the child's terminal control before appending the parent's completion, so the
completion can consume the reserved slot even when the ordinary mailbox prefix
is full. If a descendant terminal commits but the following ancestor scan fails,
the Kernel wakes its settlement worker and also retries waiting Activations on a
five-second fallback interval. An in-process transient Store failure therefore
does not require a process restart to finish durable ancestor settlement.

The Store derives every child's root from its durable parent Header and rejects
a conflicting declared root. Message admission likewise derives the target's
root rather than trusting a caller-supplied label. This keeps tree size, ready
ordering, and descendant accounting on one immutable lineage.

## Alternatives considered

Keeping children as in-process tasks or Meta child Fibers was rejected because
generation cleanup would become Session loss and live ownership would replace
durable history. Disabling fork in the first version was rejected once subagent
continuation became the requested interface; a fresh child remains available as
the explicit `none` selection.

Reusing replay solely because a current profile accepts the same namespace and
version was rejected: the saved `ModelIntent` route and the current prepared
route can differ in deployment, endpoint, generation, or credential. The
current `LanguageCall` seam exposes those exact facts only in the prepared-call
snapshot, after the request has already been built. The safe current contract is
therefore visible canonical continuation without private reasoning. A future
two-stage route preflight may restore exact-route replay without weakening this
boundary. Reusing the parent's Session or truncating its Context was rejected
because fork must create a new identity and must not mutate canonical parent
history.

Passing Session or tree authority in Tool arguments was rejected because model
output is untrusted. Reserving completion capacity at spawn was rejected because
one durable child may have multiple cold Activations. Holding an executor lane
while waiting for descendants was rejected because supervision would consume
execution capacity. Clearing accepted descendant inbox messages during cascade
was rejected because cancellation of live work is not revocation of earlier
durable admission.

## Consequences

Child identity, lineage, messages, completion, and control remain discoverable
without child residency. Root clients can enumerate the durable tree and route a
live approval only to the exact descendant Session that owns its subject. A
failed parent can publish a terminal Turn before its Activation settles, and an
idle parent may be woken solely by a child completion.

A cold child must still page and fold the selected parent interval. The
terminal-prefix digest lets replay revalidate the immutable selection once at
its initial cursor instead of repeating a whole-prefix selection query for each
page, but it cannot synthesize inherited model context. `fork_turns=all`
therefore performs work proportional to
the inherited prefix while retaining bounded pages and memory. Avoiding that work
would require a new immutable fork-seed object with retention,
reference-integrity, and Store transaction contracts; exact replay would also
require the route-preflight seam described above. An ordinary replaceable
Context checkpoint is not authoritative enough.

After the child's first Turn is terminal, checkpoint maintenance may read that
same immutable parent interval through a terminal-claim-only Store seam and
write the first ordinary child checkpoint. Later Turns can therefore restore
the bounded cache without requiring a live claim for the already-finished fork
seed; the parent Facts remain the authority whenever the cache is absent or
invalid.

Recursive cancellation remains cooperative for safe-Rust work. Restricted Linux
process execution supplies OS containment, while an unconfined descendant that
escapes its process group can outlive hard process death as recorded by the
Session Host decision.
