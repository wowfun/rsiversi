# rsi-agent-protocol

This crate owns a version-one semantic tool-control contract: closed JSON
envelopes plus RAT1 binary chunks. No active runtime or plugin currently maps
the contract to `rsi-meta` service streams. A future ordinary plugin may use
it without changing its provider-neutral meaning. AI requests, streaming
events, results, errors, freeform grammar definitions, and media invariants are
owned by [`rsi-ai-protocol`](../../rsi-ai/protocol/README.md). This crate
re-exports the shared freeform type and validates tool-image metadata through
the shared media descriptor; it owns only the tool-stream blob identifier and
wire shape layered around those semantics.

All untrusted tool envelopes enter through `ToolsEnvelope::decode`. It rejects
duplicate keys and numbers that cannot survive `serde_json::Value`
materialization, then enforces protocol identity, wire version, closed DTO
shape, field and collection bounds, recursive JSON limits, and the 768 KiB
DATA-envelope ceiling. `encode` emits recursively key-sorted JSON.

The protocol models one stream as belonging to one live owner epoch.
`owner_open` binds its immutable
execution cwd and tool-policy SHA-256 before catalog or invoke traffic. `cancel_invoke` requests
quiescence but has no independent terminal acknowledgement: the original
`invoke_response` remains authoritative. A provider retains each result and
any producer cursor until `commit_result` proves an owning runtime durably
recorded it.
Owner-scoped `notification` frames carry quiet or wakeup completion delivery.

Results may carry bounded private `AppliedPatchDelta` provenance intended for
an owning runtime to persist adjacent to the semantic result without projecting
it to a model. It
records ordered path and digest observations for changes known to have
committed, including a committed prefix returned with a later patch error.
Every provenance path is a normalized, nonempty, control-free relative path:
absolute roots, platform prefixes, empty components, `.`, `..`, and Unicode
control characters are rejected at the typed protocol boundary. Components
also reject Win32 device basenames,
including their extension and superscript-digit forms, so one typed path has a
portable ordinary-file interpretation.

Successful results contain ordered native text/image blocks. Image bytes are
opened by `blob_start`, transferred in independently validated 256 KiB RAT1
chunks, and closed by `blob_end`; an invoke result may reference only a
complete blob. The provider sends no base64 media in JSON. Artifact validation,
lifecycle correlation, and durable CAS publication remain obligations of a
future owning runtime and are not claims of this crate.

The [tools schema](../../../schemas/rsi-agent/tools-envelope.schema.json) owns
the closed JSON shape and expressible field bounds. Aggregate catalog/result
text bytes, envelope bytes, duplicate tool names, media invariants, and
recursive JSON depth/node limits remain decoder invariants. Raw tool arguments
stay strings for audit; `parse_json_strict_f64` rejects lossy machine numbers,
canonicalizes exact integral values within the JSON integer representation as
integers, accepts larger mathematical integers only when their full decimal
value equals the selected finite `f64`, and produces the same
bounded value used for schema validation and dispatch. A semantic tool may
carry one optional Lark grammar of at most 64 KiB UTF-8 alongside its canonical
object schema so language adapters can project the same call as freeform or
function-only syntax. JSON Schema supplies the expressible Unicode-scalar
ceiling; the decoder owns the exact byte ceiling.

Applied patch provenance permits `overwritten_sha256` only for add and move
changes. Update changes already identify the prior content with
`before_sha256`, so accepting a second overwritten digest would create two
names for the same observation.
