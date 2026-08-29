# rsi-agent-executor

Ordinary executor plugin over exact Turn execution/finalization, Language,
Image, Media, and Tool Local contracts. Every provider or Tool attempt is
prepared, recorded, flushed, marked started, flushed again, and only then
invoked. Image outputs enter Media and each ref is durably flushed before the
stream advances; later failure preserves those refs in `partial_failed`.
Every exact-prefix durability wait has a validated executor-local deadline, so
a persistently unhealthy Store cannot occupy that executor indefinitely.
The complete pre-terminal finalizer snapshot has a separate validated deadline;
expiry becomes `turn.finalization_timeout` and the sole durable turn failure.

The executor delegates all prompt reconstruction and compaction to
`rsi-agent-context`; it never reads a Workspace implicitly. Effect-owned
pre-terminal finalizers run before the sole terminal Fact.
If an executor reclaims history containing a completed Model event but no turn
terminal, it records interruption rather than repeating the completed external
effect.
