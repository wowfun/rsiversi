# rsi-agent-executor

Ordinary executor plugin over exact Turn execution/finalization, Language,
Image, Media, Approval, Sandbox, and Jobs Local contracts. Tool authority is
not a standing executor dependency: each exact claim supplies its resident
Agent-composition pin and immutable Tool catalog. Definitions, prepare,
retained-result recovery, and commit all use that one catalog. Any admitted
Tool retained past the main driver future carries a clone of the generation
pin, so teardown cannot destroy the catalog or its hidden Scope while delayed
work is settling. Every provider or Tool attempt is
prepared, recorded, flushed, marked started, flushed again, and only then
invoked. Image outputs enter Media and each ref is durably flushed before the
stream advances; later failure preserves those refs in `partial_failed`.
For one parallel-safe run, the executor publishes the complete source-ordered
intent batch and crosses one durability barrier, then publishes the matching
source-ordered start batch and crosses one durability barrier before invoking
any member. This preserves each call's intent-before-start proof without
multiplying startup barriers by the batch cardinality. Result publication stays
per call because budget rejection and retained-result commit are independently
owned by each identity.
Every exact-prefix durability wait has a validated executor-local deadline, so
a persistently unhealthy Store cannot occupy that executor indefinitely.
One executor registration owns a bounded pool of claim lanes. The reusable
configuration defaults to one active turn and accepts `1..=256`; standard
composition explicitly selects four. Kernel claim authority remains the sole
scheduler: different Sessions may occupy separate lanes, while the Kernel
keeps at most one active turn per Session and preserves its durable terminal
gate. The pool dispatches one task per claimed Turn behind a shared admission;
an idle Kernel claim wait does not reserve execution admission, and at most the
coordinator's next claim can wait for a lane. If shutdown wins that wait, the
coordinator releases the claim back to the Kernel. A lane explicitly reclaims
its permit and closes its parking authority when the Turn driver ends, even
when a retained Tool still holds the typed execution extensions.
`wait_agent` releases that admission while its durable wait is parked and
reacquires it before returning a result, so waiting descendants do not consume
the fixed execution capacity. Reacquisition is bounded by the Tool execution's
cancellation; cancellation exits without publishing a result when admission is
unavailable. Tools that can park declare `exclusive_final`
scheduling. The executor rejects such a call before publishing any Tool effect
unless it is last in provider source order, and injects the lane-parking
authority only into that scheduling class. `Exclusive` and `ParallelSafe`
calls cannot observe or release it, so a parallel batch never shares one
lane-parking authority. A single activation-level
coordinator owns lane failure, shutdown, retained-Tool cleanup, and the one
absolute shutdown deadline; an individual lane never performs global cleanup.
When publication reports `FlushRequired`, the executor flushes the current
live prefix and retries with the exact returned owned bodies. The normal
publish path does not clone bodies in anticipation of this uncommon branch.
Kernel shutdown during publication stops the driver without synthesizing an
`executor.internal` terminal outcome that the closing Kernel cannot accept.
The complete pre-terminal finalizer snapshot has a separate validated deadline;
expiry becomes `turn.finalization_timeout` and the sole durable turn failure.
Finalization resolves the outcome before any budget marker is published, so a
finalizer failure cannot conflict with an already-published terminal class.
If an executor reclaims a durable exhaustion marker after a terminal flush
failure, it publishes that marker's exact terminal class without invoking the
pre-marker finalizer snapshot a second time.

The executor delegates all prompt reconstruction and compaction to
`rsi-agent-context`; it never reads a Workspace implicitly. Effect-owned
pre-terminal finalizers run before the sole terminal Fact.
If an executor reclaims history containing a completed Model event but no turn
terminal, it records interruption rather than repeating the completed external
effect.

The immutable session settings supply the mandatory elapsed, provider, Tool,
generated-Fact, and generated-byte budget. The elapsed deadline bounds the
complete driver future, including provider preparation and Media import; it
drops a non-cooperative caller future at expiry while effect owners retain any
separately documented blocking-task cleanup. An already-terminal drive result
wins when it becomes ready in the same scheduler poll as the elapsed deadline;
the deadline only replaces a drive that actually stopped on cancellation. An
admitted Tool identity is
tracked outside that dropped future; after elapsed terminal durability, the
executor transfers settlement to a bounded background retirement task, so an
uncooperative Tool cannot block the single claim loop. Settlement commits the
retained result; executor shutdown or `retained_tool_wait_ms` expiry drops the
task's exact generation pin without claiming that arbitrary Tool code stopped.
Recovery registers a durable `ToolStarted` identity and its resident generation
pin in the same tracker before awaiting the retained result, so an inherited
elapsed deadline cannot retire that generation or lose the identity while the
Tool is still settling.
Other exhaustion paths stop
further admission and then persist a typed exhaustion marker plus the sole
terminal outcome. If a settled Tool result cannot be published because that
publication crosses a budget, the executor retains that first publication
failure but still attempts every later already-settled sibling in source order;
a successful sibling is published and committed before the first failure is
propagated. The failed result's retained identity is committed only after the
terminal prefix is durable. After a terminal prefix becomes durable, the
turn driver submits a bounded checkpoint request. A single owned background
writer coalesces the latest request per Session while preserving FIFO across
Sessions, incrementally rebuilds from the last
valid cache, encodes queued-turn state, and performs the optional Store write
outside the turn-critical path. Cache read, validation, rebuild, and write
failures fall back to canonical Fact replay. Restore also falls back when the
checkpoint reaches the claimed turn's acceptance sequence, preserving the
Kernel's filtered claim horizon. The scheduler retains at most 256 pending
Session keys; updating an existing key remains admissible at capacity, while a
new key is declined without evicting another Session. Checkpoints remain an
optional cache and never replace Fact durability. Executor shutdown first
settles every claim lane, then closes checkpoint admission and drains already
accepted requests within the executor's existing absolute shutdown deadline.
