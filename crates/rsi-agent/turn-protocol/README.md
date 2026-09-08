# rsi-agent-turn-protocol

`SessionCommands` is an independent Local service published by the same Kernel.
Listing and execution consume Kernel-issued resume authority, retaining the
resident or validated cold generation. Query reads the canonical request receipt
without executing a callback. Execution joins an identical in-flight request,
checks a committed receipt before callback dispatch, captures the exact control
revision and complete typed states, then runs the callback outside framework
locks. Preparation and callback execution share a 30-second deadline, with at
most 64 concurrent distinct requests per Kernel. A changed invocation using the same request ID is
a conflict; callbacks are never automatically retried after revision conflicts.
After validation, the Kernel owns the state-only commit through reconciliation
even if the caller disconnects. Commands do not create Turns, execution Facts,
Workspace registrations or external effects.

`SubmitMessage.delivery` is immutable ingress intent: fixed next Turn, fixed
next Step, or Human steering. The Kernel resolves steering atomically while
holding Session submission admission. A receipt means durable acceptance; it
does not claim that an active model request has already consumed the message.

Process-local application and executor contracts for Agent execution. Product
callers admit durable mailbox messages, read their indexed pending/claimed/
discarded state, observe independent Agent-control and Fact streams, cancel an
unclaimed message or claimed Turn, and submit direct Image turns. Mailbox claim
creates the Language Turn and first Step atomically; callers never speculate a
Turn identity at message acceptance. Fresh submissions consume a
prepared session carrying the exact Agent-composition generation selected by a
process-local draft. Resume first obtains a move-only prepared token from the
same Turn service; that token carries the authoritative Header and its exact
resident or current-cold generation pin, never a caller preset override.
Applications acquire it before durable Workspace registration, and submission
consumes it. A token dropped after later application validation fails releases
its pin without loading resident state. Agent wait durations are exact
millisecond inputs within `1ms..=1h`; sub-millisecond direct API values are
rejected instead of being rounded into a zero durable deadline.
Mailbox submission carries no caller-declared tree root. The Kernel derives the
root from the prepared Header and persists only that authoritative lineage.
Executors register and claim work, obtain the claim's immutable composition
pin, publish ordered Facts, and wait for explicit durable watermarks before
external I/O. Delayed Tool work must retain that exact pin rather than consult
a process-global mutable catalog. Every executor implementation must explicitly
provide fork replay, next-Step admission, workspace refresh, Step closure, and
activation settlement behavior; these durable lifecycle hooks never default to
silent no-ops. Dropping an observation is detach.

Source mutation requests carry the Tool execution cancellation token separately
from model JSON. Final source admission and retained commit ownership follow the
[Kernel contract](../kernel/README.md). `settlement_health` reads bounded runtime
settlement diagnostics without Store I/O.

`commit_domains` accepts exact-generation typed proposals and optional Fact
bodies without a caller-selected source or charging category. Kernel assigns
the claimed Turn, validates the mixed candidate and retains commit ownership.
Its canonical receipt supports exact idempotent retry and read-only query after
a lost acknowledgement. Opaque state/history reads do not require execution
codecs. An indeterminate commit whose receipt also cannot be read closes Session
execution until durable recovery establishes its state.

`finish_turn` owns the durable ending transaction for direct and mailbox Turns,
including any open-Step closure, required budget marker and tree settlement.
The executor supplies the resolved outcome and waits for that terminal receipt;
it does not independently publish an intermediate ending prefix. Ordinary
`close_current_step` remains charged business publication.

Mailbox admission, message state, dual-stream reconnectable observation, and
the six source-authorized Agent operations share this seam. Spawn creates a
durable continuable fork child; send/followup address only a direct parent-child
edge. Send has a fixed next-Step horizon and remains held while the target is
idle; followup always queues a waking next Turn, even when one is already
running. List and wait observe descendants; interrupt cancels only the target's
current Turn. A wait classifies a changed descendant as completion only from
the exact changed control interval, paging through that interval when it exceeds
one Store page. If every current descendant is already idle (or none exists),
the call performs one revalidated observation and returns `NoProgress` or
`Changed` without recording a park/resume pair because no live supervision wait
began. Cancellation while a durable wait is parked records a cancel-caused
resume and returns `TurnError::Cancelled`, rather than classifying cancellation
as malformed Tool input. A cancelled wait returns without reacquiring execution
capacity. Its caller must end execution and may only settle already admitted
effects and publish the terminal outcome; it cannot start further effects with
that wait's released lane. The standard executor maps cancellation/timeout to a
terminal drive failure. An unforgeable `AgentCallerAuthority` is derived from a live
claim and transported to trusted Tools through the generic typed extension
slot, so model arguments cannot invent tree authority. The legacy direct
Language-turn method remains a lower-level test and recovery seam; the standard
Session product enters Language work through mailbox claim. A next-Step
completion still pending when its parent's activation Turn ends is durably
promoted to a waking next Turn rather than being stranded. Ordinary
fixed-horizon next-Step messages remain held. Dual-stream
Session observation returns durable records after exact independent cursors, while the
older Turn observation seam retains live-first delivery for an already-known
direct Turn.
Each message receipt also carries the durable Fact tail observed with that
receipt. Together with the acceptance control cursor, it is a reconnectable
starting point that lets a caller subscribe for the later claim without
replaying old Facts or polling message status. A claimed receipt proves its
model-visible input Fact is at or before that observed tail.

Message receipts/states, observation cursors and cancellation values have closed
Serde representations for domain adapters. Decoders still validate cross-field
receipt and stream invariants at their owning boundary; these values do not
change the durable Session or Store format.
Each claim exposes only borrowed getters and carries a private Kernel-issued
seal plus the resident session's shared immutable Header allocation. Live
operations require that seal, current claim identity, and the exact resident
Header allocation; callers cannot fabricate fields or obtain the internal
`Arc`. The typed Header is validated at its construction or durable decoding
boundary, and claim issuance does not serialize it again. Post-terminal
checkpoint maintenance retains only a clone of the immutable claim because terminal
commit has already retired live claim state.

Nonterminal Facts are live-first and carry the durable watermark that existed
at publication. The sole terminal Fact and `outcome` become visible only after
the terminal Fact's complete prefix is durable. A permanent flush failure ends
an attached observation with `TurnError::Flush`; observation cannot wait
forever for a terminal that the Store can no longer commit.
Publication, claim pages, and Store append suffixes share immutable
`Arc<SessionFact>` allocations. Observations use concrete `ObservedFact` and
`ObservedControl` handles, exposing borrowed records without an escaping bare
Arc. Clones share both payload and retained-byte reservation; only the last
clone releases that item's reservation, even after its stream has been dropped.
An adapter's `ObservationRetention` pool admits each complete page atomically
and returns `Capacity` immediately when it cannot retain the page. More than
the protocol's 512 records is `Invalid`, because releasing capacity cannot make
that page valid. Failed admission advances no delivered cursor. Consumers reconnect from their last
actually delivered cursor after releasing earlier items. Encoded canonical
payload bytes are charged conservatively per observation; this is independent
of Store-read materialization and does not claim a total heap or RSS limit.
The standard pool is 64 MiB and can only be tightened while admitting one
maximum Fact. Transport decoding and renderer queues preserve these handles.
Publication consumes owned Fact bodies. `Published` returns shared Facts only
after live commit; `FlushRequired` returns the canonical unpublished bodies for
an explicit durability flush and retry. Terminal canonicalization may therefore
be visible to that retry even though nothing entered the live interval.
`TurnError::Flush` is reserved for a
real durable flush or shutdown failure, never ordinary pending-capacity
backpressure or Store reads. A Store failure outside a requested durability
barrier is reported separately as `TurnError::Store`.

The executor-facing seam also carries optional opaque Context checkpoints and
a maintenance-only unfiltered durable Fact page available only after the
claimed turn is terminal and the live tail is fully durable. This lets Context
encode deterministic queued-turn state. Each claim carries its own acceptance
sequence; restore accepts only a checkpoint ending before that sequence, so a
maintenance cache that already folded the claimed or later turns falls back to
claim-filtered canonical replay.
They are installed only behind the Kernel's terminal/durable-tail checks; cache
misses and rejected writes are explicitly non-fatal. The seam carries the
Context-computed Fact-prefix digest separately from the opaque bytes so restore
can require both views to agree.

The Kernel-owned finalization registry accepts an exact-generation
`RegistrationContext` and installs undo before publication. Loading registrations
join setup rollback; Active registrations own dynamic effects. At most 64 hooks
may be registered. The Kernel factory binds the registry to its Runtime; a
standalone Kernel binds on its first successful registration. This identity
persists until Kernel shutdown and never combines different Runtimes.

The registry reuses an immutable membership/order snapshot while both are
unchanged. Child declaration positions determine precedence; rebuilding at the
same position preserves precedence, and reorder changes no Fiber generation.
Retirement removes a hook from new snapshots; an already captured invocation
keeps its complete snapshot. Hooks run without registry or lifecycle locks.
The registry starts that complete snapshot concurrently. A hook
receives the exact turn identities and its opaque Jobs scope authority. Each
hook returns an optional completion blocker; panics and errors are isolated,
and the registry selects the first cleanup error or blocker by declaration
order only after every hook settles. The executor applies one deadline to the
complete snapshot. Timeout outranks cleanup error, which outranks a completion
blocker, which outranks the original outcome. Cleanup failure replaces every
non-success outcome; a blocker replaces only `Completed` or `PartialFailed`.
Blocker messages use the durable diagnostic byte and NUL/DEL safety contract.
Finalizer-owned reaping must outlive a dropped executor wait.

The executor reads its deadline through the Kernel's read-only `ElapsedBudget`
watch. A watch reports consumed execution milliseconds and waits for exhaustion;
it confers no authority to pause, extend, or reset a Turn budget.

`park_human_wait` consumes an explicit generic Tool lane-parking authority and
returns one single-use `HumanWait`. The Kernel owns the complete park/resume
sequence, including its tree permit, the caller's opaque executor permit, and
the budget clock. Dropping a resume waiter does not cancel the admitted cleanup
operation. No executor implementation or semaphore crosses this interface.
A human wait is owned by a Kernel task from admission through cleanup. Dropping
its handle only closes a channel and is safe outside a Tokio runtime. Claim
release, executor withdrawal, and Kernel shutdown also request cleanup. The
retained mutation lease authorizes only completion of the admitted wait after
live claim authority is withdrawn; shutdown drains that completion before its
final flush. A terminal Turn cannot start a human wait.

`ready_health` reports cumulative ready-scheduler failures and the latest bounded
diagnostic without Store I/O. The diagnostic remains available after recovery;
transient enumeration and per-root failures retry without withdrawing executors.
