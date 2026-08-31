# rsi-agent

`rsi-agent` owns durable Agent sessions. Session, Store, Turn, AI, and Tool
contracts have separate owners; the product has no umbrella wire protocol and
no integration crate that bypasses ordinary `rsi-meta` composition.

The session protocol owns immutable headers and append-only Facts. Store owns
mechanical persistence and content-addressed media. Kernel owns live turn state,
write-behind, recovery, observation, and cancellation. Context owns deterministic
Fact-to-model projection and compaction. Executor owns effect ordering across
Language, Image, and Tool calls plus pre-terminal finalization. SQLite is one Store plugin, while testkit supplies a
deterministic memory implementation. Presets contribute bounded Profile
sources. Composition builds immutable standing Agent generations inside the
same Runtime, and session or draft pins keep their exact Tool catalog alive
without exposing provider catalogs to Kernel or Executor.

All runtime components are ordinary plugins over public `rsi-meta`
`Runtime -> Context -> Fiber` semantics. `rsi-meta` remains unaware of Agent
concepts, and provider semantics remain owned by `rsi-ai`.
