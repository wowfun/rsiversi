# rsi-agent-turn-protocol

Process-local application and executor contracts for Agent turns. Application
callers submit Language or direct Image turns, cancel, observe, inspect
outcomes, and read immutable session headers. Fresh submissions consume a
prepared session carrying the exact Agent-composition generation selected by a
process-local draft. Resume first obtains a move-only prepared token from the
same Turn service; that token carries the authoritative Header and its exact
resident or current-cold generation pin, never a caller preset override.
Applications acquire it before durable Workspace registration, and submission
consumes it. A token dropped after later application validation fails releases
its pin without loading resident state.
Executors register and claim work, obtain the claim's immutable composition
pin, publish ordered Facts, and wait for explicit durable watermarks before
external I/O. Delayed Tool work must retain that exact pin rather than consult
a process-global mutable catalog. Dropping an observation is
detach; there is no fork operation.
Each claim also carries a private Kernel-issued binding over every public claim
field. Live operations require both that binding and current ownership;
post-terminal checkpoint maintenance retains only the binding because terminal
commit has already retired live claim state.

Nonterminal Facts are live-first and carry the durable watermark that existed
at publication. The sole terminal Fact and `outcome` become visible only after
the terminal Fact's complete prefix is durable. A permanent flush failure ends
an attached observation with `TurnError::Flush`; observation cannot wait
forever for a terminal that the Store can no longer commit.
Every process-local Fact seam uses `Arc<SessionFact>`: publication, claim
pages, and observation share one immutable allocation while the Store remains
the serialization owner.

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
