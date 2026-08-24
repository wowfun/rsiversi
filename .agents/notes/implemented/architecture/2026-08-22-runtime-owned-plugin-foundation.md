---
name: Runtime-owned plugin foundation
comment: Minimal Context and Fiber core with native loading as an ordinary plugin
---

## Problem

The former `rsi-meta` surface combined composition files, a daemon protocol,
persistence, recovery, native framing, and product integrations into one
platform. Those layers duplicated lifecycle authority and made ordinary plugin
behavior depend on orchestration machinery. The repository also retained Agent
and AI integration claims after their host contracts had been removed.

The foundation needs one ownership model for setup, dependency convergence,
calls, events, retirement, and shutdown. Cancellation or a hostile trusted
native callback must not let a caller-owned future strand shared lifecycle
state or permit overlapping access to foreign mutable state.

## Decision

The active model is `Runtime -> Context -> Fiber`. Core owns bounded registries,
exact generation and contract fencing, dependency convergence, application
reconciliation, callback admission, listener authority, effects, and joinable
shutdown. Long-lived lifecycle work belongs to Runtime-owned tasks, so dropping
the initiating future does not abandon published state. Cleanup waits for
admitted safe-Rust work instead of freeing resources after a deadline. A
provider gate stores closure and the admitted-callback count in one atomic
state; separate atomics would not give retirement and ordinary concurrent
admission a shared linearization point. Cleanup-time calls from retiring
dependents remain ordered by the convergence transaction that provider
retirement joins before draining.

Each service invocation has one Runtime-owned call driver. The driver owns the
real channel halves, deadline, terminal status, and provider-generation lease;
an endpoint receives only a borrowed `ProviderChannel`, so safe Rust cannot
detach the channel beyond `serve`. A single explicit terminal distinguishes
clean EOF from provider error or panic. The driver alone retains the provider
generation lease through endpoint exit and terminal publication. Driver and
caller share Runtime and service-call admission until the driver has stopped
and the caller observes that terminal or drops the call, so buffered but unread
responses remain accounted without delaying provider teardown.
Endpoint exit includes destruction of the endpoint future: cancellation or
deadline first drops that future, then publishes the terminal and releases the
generation lease. Each Fiber retains the time-enabled executor captured when it
was inserted. Synchronous call opening uses that authority for its driver rather
than probing ambient Tokio state, and the host keeps it alive through Fiber
disposal and Runtime shutdown. This per-Fiber authority avoids binding one
Runtime globally to the executor of its first application. Plugin-returned failures are normalized
to the service boundary before they merge with genuine driver terminal,
cancellation, and deadline outcomes; event handlers follow the same rule for
their event boundary.
Queued request and response frames hold weighted permits from one Runtime-wide
logical byte budget and release them when the receiving side consumes them.
Admission is work-conserving for mixed frame sizes until an older frame reaches
a bounded bypass count; it then becomes the reservation barrier, preventing
permanent large-frame starvation. Every admission paired with a logical resource
ledger drops the ledger reservation before returning capacity, preventing a
woken waiter from observing transient false capacity at exact saturation.

Application ownership transfers only after the initiating future acknowledges
the returned handle; cancellation before that acknowledgement makes the
Runtime-owned task dispose the Fiber. The ownership guard enters that Fiber's
captured executor when it is destroyed on another thread, so scheduler handoff
cannot depend on the drop thread's ambient runtime. Terminal state is also a publication
fence: an activation that finishes after terminalization is rolled back rather
than becoming Active.

Everything outside that kernel is a plugin. Native discovery and execution are
implemented by the ordinary Loader plugin over `PluginFactory`; core has no
native or product-specific vocabulary. The v1 C ABI is a small synchronous,
byte-oriented call boundary with explicit ownership and panic containment.
Minor-version extension appends fields: compatibility is decided from the
minimum prefix for the table's declared minor rather than the newest host table
size. The loader zero-initializes plugin output storage before entry and pins the
verified artifact identity. Operation-specific native timeout semantics are
owned by the [loader contract](../../../../crates/rsi-meta/loader/README.md).
Create and service-call callbacks use atomic completion-or-timeout adjudication;
the timeout winner publishes factory or instance poison and the owning Runtime
terminal fence before completion can release the gate. Pre-application
configuration validation has no Runtime authority and poisons only the factory,
catalog load is fenced by digest and cache lease, and admitted destruction has
no adapter-local deadline. All timed-out foreign work retains its thread and
owned resources until it actually exits.

Terminalization covers the complete Runtime. A trusted in-process callback
shares memory and may have damaged global invariants, so fencing only its module
would falsely imply a fault-isolation boundary. Dedicated foreign threads keep
hung callbacks off Tokio's shared blocking pool. A catalog-owned executor
acquires callback admission before thread creation and owns a separate bounded
destruction lane, so Loader and direct catalog users share one native resource
authority rather than supplying independent limits.
The synchronous service ABI uses one admitted worker per frame, keeping its
deadline local and releasing the instance gate and callback permit while a
stream is idle; frames for one instance are intentionally serialized rather
than pipelined.
Factory validation and create serialization fail fast at their shared gate;
prepared applications in multiple Runtimes therefore cannot form a hidden
cross-Runtime waiter queue before executor admission.
The worker and mapped module also retain the catalog cache lease through
foreign teardown. Source identity is hashed before staging so a live mapped
digest needs no second private copy; a miss is read again into stable staging,
whose digest becomes authoritative. The durable commit recomputes that staged
digest while making its private synced commit copy. Unix catalogs pin and lock
the cache-directory object as their primary ownership authority, so unlinking
the cooperative marker cannot create a second Catalog for the same directory.
Enumeration, private temporary files, digest opens, publication, comparison,
rollback, and durability all resolve relative to that object. Public-path
revalidation only poisons later admission after replacement and is never the
cache I/O authority;
the Windows marker lock denies delete sharing and prevents replacement while
owned. The catalog also verifies the commit pathname
still names the synced open file before publication and compares the published
durable name with the staged artifact before accounting. Windows staging closes
its exclusive writer, reopens read-only with write/delete sharing denied, and
rechecks the digest before mapping. Linux durable accounting
linearizes only after the final directory sync; failure removes the published
name relative to the pinned directory and durably syncs that same object rather
than any pathname replacement, while an unprovable cleanup permanently poisons
later Catalog admission. Callback permits and activity counters cover result delivery,
delivery-failure destruction, and closure exit rather than only the foreign
function body; activity decrements before the permit becomes reusable, so an
exact-limit handoff cannot inflate the reported active or peak population. Each mapped factory reserves its finalizer slot before native entry,
so ordinary queue saturation cannot leak its mapping, staging reservation, or
cache lock. Create separately reserves a catalog-wide live-instance slot before
its callback thread exists and retains it through the real destructor.
Instances dropped by publication failure or direct release of all Runtime
owners transfer that slot into a physically bounded reserved queue, avoiding
inline teardown, task-per-instance spawning, and a hidden unbounded backlog.
Registered cleanup joins the actual foreign destruction; the Runtime shutdown
deadline limits its waiter rather than canceling or prematurely completing that
work.
Catalog load admission begins before staging. Dynamic Loader commands acquire
that lease before ID mutation or any task/thread creation and retain it through
response delivery or rollback, bounding cancelled digest-gate waiters without
introducing an internal FIFO.
Initial preflight also acquires this shared authority before `spawn_blocking`;
its mapping width is capped by both eight and the Catalog's validated load
concurrency. After bounded mapping, entries are grouped by module digest: each
fail-fast factory gate is entered in configuration order, while distinct modules
normalize only up to the Runtime's preparation limit. These buffers remain local
scheduling bounds rather than competing admission authorities, and therefore
cannot make one valid batch self-reject at either shared fail-fast bound.
Dynamic Loader IDs use one token-fenced slot through claim, publication, unload,
response acknowledgement, rollback, and release, so stale completion cannot
remove a newer generation that reused the same external ID.
Loader command responses serialize through the Runtime frame budget and
replace an oversized partial encoding with one fixed bounded diagnostic;
inspection clones the bounded handle map under the Loader registry mutex, then
constructs snapshots and consumes them one at a time after releasing that lock.
Factory finalization remains catalog-owned: `NativeFactory` can be shared or
retained outside a Runtime, so Runtime shutdown cannot safely claim or destroy
the last ABI factory. The cache lease is the finalization fence for that
external ownership domain; Runtime cleanup joins only registered instances.

Initial native configuration crosses core's opaque prepared-application seam,
so transformation happens exactly once before any child is published. A proof
is bound to the Runtime that validated it and owns reservations for the Fiber,
retained plugin bytes, service declarations, and dependency edges until it is
applied or dropped. Runtime policy is grouped by topology, payload, execution,
and deadlines and validated into an in-process trusted form at construction;
JSON depth cannot exceed the implementation-safe hard ceiling of 128 because
shape traversal is iterative but compact encoding uses a recursive serializer;
preparation and reconfiguration use fail-fast admission rather than retaining
an internal waiter queue. Reconfiguration additionally holds a separate
maximum-sized configuration reservation through normalization, shrinks it to
the validated result, releases the previous `Arc<Value>`, and then transfers
the staging reservation without a second capacity increment. Disposal does
not wait for this reconfiguration gate while holding the Fiber transition;
instead it linearizes by setting `disposed` under the Fiber data lock, after
which an in-flight normalizer either observes the fence before publication or
joins a ticket completed as disposed. Context overlay accounting includes each
retained per-service encoding rather than only its JSON values, including JSON
escaping in the quoted service key. Every owned configuration, intercept, and
event input is guarded before fallible work or async future construction, and
plugin-normalized configuration plus event-handler output are guarded as soon
as they return. The guard dismantles rejected or unpolled values iteratively;
validating by reference alone would otherwise leave recursive
`serde_json::Value` destruction exposed to adversarial nesting. Cleanup reports
keep their invariant-bearing state private and expose immutable observations,
so callers cannot make `is_clean`, retained failure entries, and serialized
metadata disagree. Public resource snapshots describe these logical budgets,
not allocator RSS. The
bounded pending-state contract stores a `PendingReport` rather than an
unbounded reason vector: top-level reasons and dependency-cycle service samples
share the configured diagnostic entry and UTF-8 byte limits, while total and
truncation metadata preserve omitted evidence without first materializing a
full cycle path. The
active `rsi-ai` product remains standalone, and the active `rsi-agent` product
surface is its protocol. The superseded runtime decisions are retained only in
the archived `2026-08-18-replayable-agent-turn-runtime.md` and
`2026-08-21-live-agent-and-coding-tools.md` notes. This decision also supersedes
the native composition and durable Agent integration portions of the archived
`2026-08-19-five-capability-ai-boundary.md` note while retaining its
provider-neutral standalone SDK decision.

External operations also cross one Runtime-wide closeable admission gate.
Preparation transfers its gate lease into `PreparedPlugin`; application drops
it only after registry insertion, while reconfiguration, service calls, and
event dispatch retain it for their full admitted lifetime. Shutdown closes the
gate before driver cancellation and captures strong root references
immediately, so concurrent public disposal cannot erase a captured run or its
report before shutdown joins it, and independent teardown starts even when a
caller retains a proof or terminal
service call. Every captured root disposal intent is submitted before shutdown
waits for any root result, so lazy join polling cannot hide later roots from the
scheduler. It drains every pre-close lease only after disposal and scheduler
work. At that point it atomically hard-seals the gate, making even a stale
retiring caller fail acquisition; any retiring caller that won before the seal
is part of the final drain. This closes the check-to-reservation race without turning the gate into a
head-of-line cleanup barrier. Cached completion additionally requires an idle
scheduler, zero logical resources, and an empty Fiber registry. An earlier
terminal reason remains observable diagnostic evidence but does not block
`ShutdownOutcome::Complete` after those conditions hold; `TimedOut` always
means the caller can still rejoin tracked work.
When a child completes public disposal before its parent claims cleanup, the
child transfers its bounded report into the owning generation before removing
the ownership edge. Parent and shutdown reports therefore retain descendant
failures without accumulating an unbounded history of completed child handles.
User cleanup effects have their own panic boundary. A panic escaping the
cleanup driver means registry withdrawal is no longer provable, so the Runtime
records a bounded failure and terminalizes instead of permitting later
publication against possibly stale ownership.
An unexpected shutdown-driver panic or a joined disposal that cannot prove
quiescence is cached separately as a failed run; later waiters receive
`ShutdownOutcome::Failed` immediately with a fresh bounded unresolved snapshot,
while `Complete` remains exclusive to proven quiescence.
Reconciliation is cooperatively reentrant only at explicit Fiber-operation
waits: a transition waiting for another Fiber's apply, reconfiguration,
retirement, or disposal releases its global scheduler permit and reacquires it
before resuming local mutation or propagating an unwind. While released it is
counted as paused, owns no reconciliation permit or ledger reservation, and a
nested-intent guard requeues any unfinished claim on drop. Terminalization is a control fence rather than
ordinary admission, so a trusted adapter can still publish the first bounded
terminal reason after shutdown has closed the external-work gate.
Each loading generation also owns an attempt-local cancellation fence. A newer
desired revision cancels that stale attempt before provider cleanup joins its
dependent ticket, breaking the otherwise circular case where activation awaits
disposal of the provider whose withdrawal requested the same dependent revision.
Reverse dependency membership and service-change notification preserve the
complete service slot, including isolation. Collapsing that identity to a key
would make an unrelated same-named provider withdrawal join a consumer that
cannot bind it and can close a false disposal cycle.
One Runtime owns at most one reconciliation worker task; per-Fiber transitions
are in-memory futures within its bounded scheduler rather than spawned tasks.
Ticketed desired-revision publication and completion registration share the
same settlement lock, so a worker cannot settle a revision between those two
steps and strand its waiter. Once the revision counter saturates, a queued intent
still runs, but each run settles only the completion prefix captured when it
started; tickets registered during that run wait for the queued rerun.
Terminal disposal settles registered reconciliation tickets from the published
final snapshot even if the disposal driver catches a late panic after state
publication; the persistent disposal result and scheduler completion must not
diverge.
The public resource snapshot records that worker's current and peak usage so
the dense-graph probe verifies this property directly.

The transition duration is one absolute deadline for each public async apply
or reconfiguration waiter, beginning before blocking normalization. Timeout
detaches only the waiter: blocking preparation keeps its permit until the
worker returns, inserted unacknowledged applications enter their persistent
disposal run, and reconfiguration, rollback, and cleanup continue to settle.
Internal service-driven reconciliations retain an activation-attempt deadline
even though they have no public waiter. Synchronous `Runtime::prepare` remains
explicit caller-owned preflight rather than pretending a synchronous callback
can be cancelled by an async deadline.

## Alternatives considered

Keeping the composition daemon and adapting the new core beneath it was
rejected because compatibility would preserve two lifecycle authorities.
Embedding native loading, AI routing, or Agent durability in core was rejected
because each adds policy and failure modes that an ordinary plugin can own.

Emulating the removed stream/lifecycle ABI for the old coding-tools provider
was rejected. The synchronous v1 call boundary does not define durable session
identity, provider-to-host notifications, or fallible asynchronous teardown.
A compatibility shim would conceal those missing contracts rather than provide
the foundation needed to prove them.

Timing out cleanup and freeing a native generation anyway was rejected because
it can unload code or destroy data still used by a foreign thread. Forceful
preemption requires a future process or Wasm adapter, not unsafe in-process
teardown.

## Consequences

Pre-release callers receive no compatibility promise for the removed daemon,
manifests, schemas, wrappers, or frame protocol. Future AI and Agent milestones
must define their bounded plugin wires over the current public foundation and
re-establish end-to-end evidence. Obsolete Agent runtime and coding-tools
implementations are absent from the working tree so they cannot masquerade as
current architecture; archived decisions and Git history retain their
rationale.

The in-process native adapter can bound waiting and fence new work but cannot
reclaim a permanently hung foreign thread. It deliberately retains the live
instance and mapping until that callback returns. Native loader evidence is
host-specific; portable ABI checks do not imply Windows or macOS execution.
