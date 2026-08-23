# rsi-ai architecture

## Layers

`rsi-ai-protocol` owns provider-neutral meaning. `rsi-ai-provider` owns the five
provider-author adapter seams. Concrete providers translate to private HTTP,
SSE, multipart, or WebSocket syntax through `rsi-ai-transport`. The `rsi-ai`
package is the standalone façade. Concrete provider packages do not depend on
the façade, keeping the dependency graph acyclic.

The public API is capability-specific: `LanguageModel`, `ImageModel`,
`TranscriptionModel`, `SpeechModel`, and `RealtimeModel`. Image understanding
and audio understanding remain rich language inputs; image generation,
transcription, speech, and live Realtime sessions are distinct operations.
Tool execution belongs to the caller or agent orchestrator.

## Prepare and Start

Every capability follows a provider-I/O-free description and two-stage effect boundary:

```text
exact language handle -> describe(model) -> generation-pinned LanguageProfile
                                              |
                                              v
exact model handle -> prepare(validated request) -> redacted snapshot
                                               durable commit when required
                                                        |
                                                        v
                                                  start(one attempt)
```

Describe returns configured per-model context and output-reserve limits, tool
dialect, freeform and image-result support, and accepted provider-extension
formats. A production language plugin must name every model it serves together
with those three token limits; describe and prepare reject an unconfigured model
instead of inventing capacity from an adapter-wide constant. Describe performs
no provider I/O and lets an orchestrator build the exact catalog and request for
the pinned route. Prepare may validate, apply provider defaults, resolve a captured credential,
and bind local media descriptors. It must not perform inference, refresh a
catalog, fetch a media URL, or refresh an OAuth token. Start is consuming and
owns the external effect. A snapshot records deployment, provider family,
protocol, transport, endpoint fingerprint, config generation, credential
source, request digest, and bounded retry policy without secrets.

The standalone registry permits exact unlisted model identifiers because
generic compatible deployments often have no trustworthy discovery catalog.
Concrete language adapters may still require a locally configured profile for
that identifier; their bounded maps use the shared protocol-owned
`LanguageModelProfiles` invariant. Integrations that treat the returned limits
as operational facts must do so. The registry never changes a handle's route after
construction. Each adapter attempt is single shot. A future durable agent may
apply the frozen retry policy only after a durable retry event and only before
visible output.

Providers that support long-running server jobs may opt into the separate
deferred-language seam. `submit`, `poll`, `resume`, and `cancel` each perform at
most one provider request. A deferred checkpoint freezes the original prepared
route and records the remote operation identifier, terminal status,
stream-created flag, monotonic sequence cursor, and bounded provider parser
state. Each resumed stream item is an atomic pair of normalized events and the
cursor after those events; durable callers must commit the pair together.
Accumulated output is not duplicated inside the checkpoint.
This is an intentional standalone API. A future agent integration may choose
whether to expose provider-managed deferred language jobs; no active agent
runtime currently re-exports this handle.

## Normalized streams

Language uses indexed start/delta/end blocks followed by exactly one Finished
or Failed event. The assembler preserves reasoning, raw tool arguments, usage,
sources, warnings, bounded provider-private replay, and optional evidence on
reasoning input blocks. `complete`
drains this same stream.

Image and speech bytes are emitted in nonempty chunks with contiguous sequence
numbers and assembled under capability byte limits. Transcription text and
segments are ordered and bounded. Realtime validates one SessionStarted event,
bounded commands/events, monotonic input audio sequence, and one Closed event.
Dropping a generation or live session signals abort. The provider bridge never
blocks the synchronous plugin callback: it retains each Realtime input credit
until the provider task dequeues that command, so a stalled provider paces the
caller at the existing byte-credit boundary instead of failing a full internal
queue.
Large JSON media responses use the transport package's single bounded
incremental extractor. Provider adapters select a JSON Pointer and either a
string or object-array mode, then retain only the normalized envelope and one
bounded extracted chunk or item before semantic decoding. Already
syntax-validated items may be emitted before a later envelope or requested-count error; a
terminal error means consumers must discard the partial operation result.
OpenAI multipart image-edit and transcription requests stream small framing
segments around shared media owners; they never assemble a second contiguous
copy of all input media.
Production HTTP attempts have finite connect and whole-request deadlines and
observe abort while streaming the body. The OpenAI Realtime WebSocket dialer
has a finite connect deadline and observes abort during connect and socket I/O.

## Integration boundary

The five capability APIs are standalone Rust interfaces. No active package
adapts them to `rsi-meta` or an agent runtime. A future adapter must be an
ordinary plugin over the public `PluginFactory` seam and must define its own
bounded wire contract; provider packages remain unaware of that integration.
