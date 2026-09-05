# rsi-agent-turn-protocol

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
as malformed Tool input. An unforgeable `AgentCallerAuthority` is derived from a live
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
Every process-local Fact seam uses `Arc<SessionFact>`: publication, claim
pages, and observation share one immutable allocation while the Store remains
the serialization owner.
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

The Kernel-owned finalization registry snapshots effect-owned hooks in
registration order and starts the complete snapshot concurrently. A hook
receives the exact turn identities and its opaque Jobs scope authority. Each
hook returns an optional completion blocker; panics and errors are isolated,
and the registry selects the first cleanup error or blocker by registration
order only after every hook settles. The executor applies one deadline to the
complete snapshot. Timeout outranks cleanup error, which outranks a completion
blocker, which outranks the original outcome. Cleanup failure replaces every
non-success outcome; a blocker replaces only `Completed` or `PartialFailed`.
Blocker messages use the durable diagnostic byte and NUL/DEL safety contract.
Finalizer-owned reaping must outlive a dropped executor wait.
