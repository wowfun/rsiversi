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
Every exact-prefix durability wait has a validated executor-local deadline, so
a persistently unhealthy Store cannot occupy that executor indefinitely.
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
publication crosses a budget, its retained identity is committed only after
the terminal prefix is durable. After a terminal prefix becomes durable, the
turn driver only replaces a bounded checkpoint request. A single owned
background writer coalesces requests, incrementally rebuilds from the last
valid cache, encodes queued-turn state, and performs the optional Store write
outside the turn-critical path. Cache read, validation, rebuild, and write
failures fall back to canonical Fact replay. Restore also falls back when the
checkpoint reaches the claimed turn's acceptance sequence, preserving the
Kernel's filtered claim horizon.
