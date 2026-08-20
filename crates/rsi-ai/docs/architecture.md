# rsi-ai architecture

## Layers

`rsi-ai-protocol` owns provider-neutral meaning. `rsi-ai-provider` owns the five
provider-author adapter seams. Concrete providers translate to private HTTP,
SSE, multipart, or WebSocket syntax through `rsi-ai-transport`. The `rsi-ai`
package is the standalone façade; `rsi-ai-meta` adapts the same façade to
generation-pinned native services. Concrete provider packages do not depend on
the façade, keeping the dependency graph acyclic.

The public API is capability-specific: `LanguageModel`, `ImageModel`,
`TranscriptionModel`, `SpeechModel`, and `RealtimeModel`. Image understanding
and audio understanding remain rich language inputs; image generation,
transcription, speech, and live Realtime sessions are distinct operations.
Tool execution belongs to the caller or agent orchestrator.

## Prepare and Start

Every capability follows the same two-stage boundary:

```text
exact model handle -> prepare(validated request) -> redacted snapshot
                                               durable commit when required
                                                        |
                                                        v
                                                  start(one attempt)
```

Prepare may validate, apply provider defaults, resolve a captured credential,
and bind local media descriptors. It must not perform inference, refresh a
catalog, fetch a media URL, or refresh an OAuth token. Start is consuming and
owns the external effect. A snapshot records deployment, provider family,
protocol, transport, endpoint fingerprint, config generation, credential
source, request digest, and bounded retry policy without secrets.

The standalone registry permits exact unlisted model identifiers because
generic compatible deployments often have no trustworthy catalog. It never
changes a handle's route after construction. Each adapter attempt is single
shot. `rsi-agent` may apply the frozen retry policy only after a durable retry
event and only before visible output.

Providers that support long-running server jobs may opt into the separate
deferred-language seam. `submit`, `poll`, `resume`, and `cancel` each perform at
most one provider request. A deferred checkpoint freezes the original prepared
route and records the remote operation identifier, terminal status,
stream-created flag, monotonic sequence cursor, and bounded provider parser
state. Each resumed stream item is an atomic pair of normalized events and the
cursor after those events; durable callers must commit the pair together.
Accumulated output is not duplicated inside the checkpoint.
This is an intentional standalone API: the v0 `rsi-agent` retry state machine
does not submit provider-managed background jobs, so it neither consumes nor
re-exports the deferred handle.

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
Dropping a generation or live session signals abort.
Production HTTP attempts have finite connect and whole-request deadlines and
observe abort while streaming the body. The OpenAI Realtime WebSocket dialer
has a finite connect deadline and observes abort during connect and socket I/O.

## rsi-meta integration

The five version-zero service keys are `rsi.ai.language`, `rsi.ai.image`,
`rsi.ai.transcription`, `rsi.ai.speech`, and `rsi.ai.realtime`. One service
stream pins one provider generation. A language turn may execute sequential
calls on its pinned stream; each media operation and each Realtime session owns
one stream. Hot replacement affects only later streams.

The control protocol is `Prepare -> Prepared -> Start -> events -> terminal`.
JSON declares media identity, size, MIME type, and digest; raw binary chunks
carry bytes under rolling `rsi-meta` credit. Provider failures are semantic
terminal messages. Version, framing, ordering, or credit violations cancel the
service stream.

The maintained wrappers expose this matrix:

| Plugin | Capabilities |
|---|---|
| OpenAI | language Responses, image, transcription, speech, Realtime WebSocket |
| OpenAI-compatible Chat | language |
| DeepSeek | language |
| Xiaomi | transcription and speech |

## Agent ownership

`rsi-agent` asks only for a model identifier; provider instance, endpoint, and
protocol come from the composition binding and plugin config. Language model
requests and retries are transcript events. Direct media and Realtime calls use
a caller-owned `AiOperationId`; the agent database commits Prepared and Started
barriers before Start and a terminal record before returning success. Recovery
maps prepared-only operations to NotStarted and started operations without a
terminal record to OutcomeUnknown without calling a provider.

The agent workspace CAS stores validated image/audio bytes by SHA-256. Durable
records carry descriptors or artifact references, never base64 or provider
URLs. Raw Realtime frames are live-only; returned audio is committed to the CAS
and a clean Closed event terminates the durable operation.
