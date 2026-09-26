# rsi-agent-turn-protocol

`read_program` observes one run in the exact live caller's Session, including
complete verified result data and the run control revision. `cancel_program`
revokes that same Session's live run, without reviving the creator Tool. Neither
operation grants child or cross-Session control. Model adapters page the complete
rendered observation within 8 KiB by default (16 KiB maximum) and bind continuation
offsets to the returned revision; a changed run requires a fresh first page.

`ExecutionObserver` is an optional, explicitly composed effect-interval observer.
It asynchronously admits an interval bound to the exact claim, then Executor awaits
its `begin` before entering any effect owner, including Job preparation and
finalization. `end` receives both the baseline completion result and the separate
controlled-work status after the claim driver and retained Tools settle. Initial
history inspection, admission and begin share one 30-second deadline. Admission
failure prevents effects; incomplete history or baseline evidence remains partial.
Settlement wait and end share a separate 30-second deadline after drive completion.
Final observation runs outside the execution lane. The Executor reserves one of
256 observation slots before taking a lane and retains it through end completion;
capacity backpressures new claims without accumulating waiting observation tasks.
A timeout cancels its
stage token; Executor retirement also cancels stage waits without upgrading
Running work to Settled. Providers retain dispatched resources until actual completion.
Observer futures must yield cooperatively and never perform blocking I/O while
polled; deadlines cannot preempt blocking safe-Rust plugin code.
Observation never changes durable Turn outcome or gives an observer claim mutation
authority. An uncompleted baseline or unconfirmed cleanup is explicitly partial.
The contract owns ordering only; Git, summaries, storage and UI belong to callers.

`SessionProjections::resident_activity` is an instantaneous, read-only lexical
page of at most 64 resident identities and their nonterminal-work flags. It does
not read Store, resolve generations, pin sessions or acquire execution authority.
`has_more` explicitly reports an incomplete roster. A durable open Turn without
a current resident is not evidence of running work.

`TurnService::controlled_work` reads process-local Executor settlement evidence
for an exact Session/Turn. `Running` includes the claim drive and retained Tool
cleanup; `Settled` requires successful finalization and settlement of every
tracked Tool. `Unsettled` records loss of that proof (deadline, cancellation of
cleanup, or provider loss). None means no retained observation, including cold
history; neither None nor a durable terminal implies settlement. Observations
carry no execution authority. Kernel authenticates publication against the
current claim and retains at most 1024 observations, evicting only non-running
entries. Existing observers retain their exact generation across eviction or
replacement. Consumers impose their own wait deadline and must not interpret
an unknown or unsettled result as safe completion of controlled work.

`SessionProjections::resident_composition` only peeks at the current resident
generation. It does not read a cold Store, await a load or invoke composition
resolution. `NotResident` and `Loading` are explicit observations. This operation
is distinct from derived domain snapshot capture, which may resolve a cold pin.

`TurnJobs::peek_job` samples process-local output through the original executor's
weak Jobs source. The request binds Session/Header, active Turn, nonzero claim
generation, job ID and durable Tool effect. Kernel revalidates the live source
after sampling. Each stream is at most 32 KiB; their combined raw budget is
40 KiB so base64 plus bounded identity metadata fits a 60 KiB page and a 64 KiB
Session API reply, including its target envelope.
Kernel validates raw stream lengths, offsets and output references before
encoding each stream once. Typed local relays trust that result; the Session API
client validates decoded wire pages before admitting them for presentation.
Missing retained output is `None`, never scope reacquisition or execution.

Current-Turn Jobs observation is process-local. The Executor publishes a weak
read-only source under its authenticated claim, retaining the strong source only
for that claim. The source contains the exact originally acquired Jobs authority;
it exposes status listing only. Kernel reads bind Session/Header/Turn, check the
live claim both before and after sampling, and never acquire a Jobs scope.
Finalization's authority revocation, claim release and executor withdrawal make
the source unavailable. No durable replay or historical Jobs lookup is provided.
Pages follow the shared [current-Turn Jobs contract](../../rsi/session-protocol/README.md).
Cancellation is checked before and after sampling.

`SessionContinuations` is a separate Local service for trusted Host controllers.
Local lookup is not a sandbox against linked plugin code; the application-owned
contribution allowlist and its trusted implementations determine dependencies.
A bounded live
lease binds one Session Header, exact composition generation, domain and owner
identity. One retained owner per `(Session, domain)` is allowed, with 64 owners
per domain and 128 total; owners sharing a Session must share its exact generation.
Dropping the final lease or revoking it disarms further allocation and
message admission. Internal settlement may finish with the retained revoked
lease; it cannot reserve or admit input. Each submitted input binds a canonical
internal reservation receipt or the exact frozen first-publication baseline,
plus the current domain revision. Ordinary command and message routes cannot
claim this provenance. The ready scheduler and claim admission both check the
live lease and current revision; a cold pending continuation is discarded.
Pending-only discard never falls through to cancellation of a claimed Turn.
An explicit pause/cancel after restart may retain an already revoked settlement
lease. It never grants scheduling authority. Draft arm freezes an uncharged
baseline. Initial reserve evaluates the domain's pure command on a private
candidate and publishes its first allocation only with the accepted input;
Kernel does not interpret that domain's opaque JSON. Durable reserve binds the complete
input in canonical continuation command provenance and commits its acceptance in
the same idle transaction. Busy is a retryable deferral that preserves authority
and budget; the idle wait is advisory and final admission rechecks activity.

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

`SessionProjections` independently captures disposable extension views. It selects
the resident composition pin or a current cold read generation, waits for an
already admitted resident load, and rechecks concurrent publication. It never
hydrates a Session or issues execution authority. Cold projection does not require
unrelated domain codecs; a unit's semantic decode failure affects that unit only.
The Store supplies a simultaneous Fact/control cut and complete current domain
states. At most 16 captures run per Kernel, with a 30-second deadline covering
admission, generation selection, Store capture and projection. Shutdown cancels
captures; dropping a read drops its callback and resource leases.

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
claim for internal domain and supervision operations. Trusted Tools receive
`tool_caller` authority bound to their exact active effect and authenticated
source request settings; child creation requires that binding. The generic
typed extension slot carries this private authority, so model arguments cannot
invent tree authority. The legacy direct
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

`SessionProjections::watch_projection_changes` subscribes to coalesced Session
commit hints before capture, including an unpublished identity. Hints contain no
Facts, domain values or execution authority. They share the Kernel observer
count bound, terminate on shutdown, and release their registry entry on final
drop. Consumers requery a complete snapshot and compare both durable cursors;
duplicate hints never establish progress by themselves.

SessionProjections may expose an optional process-local durable activity revision
without Store I/O, observer slots or generation retention. It changes after each
Kernel commit; consumers capture it before reading durable metadata. An unavailable
or exhausted revision disables caching, and it conveys no authorization.

Fresh continuation creation is uncharged. `reserve_initial` evaluates the pinned
pure reserve command against a private baseline and atomically publishes its
Header, allocated state and exact message. An ordinary draft publication never
publishes a speculative automatic allocation. Command retries reconcile through `execute` or read-only `query`; accepted input
is observed through `message_status`. No separate input-submission phase exists.
Current lease revisions never move backwards.
Eligible owners alternate by their last accepted control position; Busy leaves
the demand pending without consuming an allocation. Idle waits subscribe to
current descendants and tree membership before rechecking state.

`ProgramToolCalls` is an executor-injected Local extension for one live coordinator
Tool. Its catalog contains only Callable definitions from that claim's pin. The
executor assigns call ordinals and identities, bounds outstanding requests to 16,
and owns settlement even if a request waiter disappears. It grants neither Agent
control nor detached authority, and it is never encoded in portable Tool input.

A foreground program policy snapshot names its exact still-started coordinator.
Kernel permits that read only while the coordinator is the sole active effect;
ordinary contribution capture retains its no-active-effect requirement. Nested
PostTool callbacks run in settled source order after coordinator settlement, so
no context input or callback domain write enters during the active coordinator.

`TurnService::prepare_program` validates an exact started workflow Tool and returns
an opaque `ProgramRun` owner. Its implementation retains the frozen composition,
parent horizon and actual Turn permission; serializable descriptors grant no
execution authority. Preparation reserves live capacity but creates no run record.
After Jobs admission, `accept` checks the creator again, then `start` records the
run before opening the external process latch. Detached operations use the owner
instead of a retired Tool claim. `detach` and creator cancellation serialize on
one run transition. Child waits read the exact initial-activation receipt and
return full verified structured output, or a bounded final public reply.

Continuation commands retain their lease after Session busy, precommit capacity
rejection, or command revision conflict, including settlement commands. Callers
may retry these typed contention outcomes with fresh state and bounded pacing.
Other failures revoke the lease; Store and unknown commit outcomes do not authorize
replay.

Initial Turn input classification follows the exact acceptance message identities
through paged claim Facts until those inputs enter or the first model intent
begins. Later steering cannot change this classification. Root-only callers enforce
lineage separately; automatic-work adapters select allowed continuation domains.
