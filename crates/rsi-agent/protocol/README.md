# rsi-agent-protocol

This crate owns the version-zero semantic DATA envelopes exchanged by
[`rsi-agent`](../README.md) with tool providers through `rsi-meta` service
streams. AI requests, streaming events, results, errors, and media descriptors
are owned by [`rsi-ai-protocol`](../../rsi-ai/protocol/README.md).

All untrusted tool envelopes enter through `ToolsEnvelope::decode`. It rejects
duplicate keys and numbers that cannot survive `serde_json::Value`
materialization, then enforces protocol identity, wire version, closed DTO
shape, field and collection bounds, recursive JSON limits, and the 768 KiB
DATA-envelope ceiling. `encode` emits recursively key-sorted JSON.

The [tools schema](../../../schemas/rsi-agent/tools-envelope.schema.json) owns
the closed external shape and expressible field bounds. Aggregate catalog
bytes, envelope bytes, duplicate tool names, and recursive JSON depth/node
limits remain decoder invariants. Raw tool arguments stay strings for audit;
`parse_json_strict` produces the same bounded canonical value used for both
schema validation and dispatch.
