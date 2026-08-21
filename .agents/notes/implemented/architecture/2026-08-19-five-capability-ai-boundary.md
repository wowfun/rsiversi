---
name: Five-capability AI boundary
comment: Provider-neutral SDK, provider seams, composition services, and durable Agent ownership
---

## Problem

The repository needs real language, image generation, transcription, speech,
and Realtime integrations without teaching `rsi-meta` provider semantics or
letting provider HTTP shapes become the Agent transcript. The boundary must
support standalone callers and native composition, preserve reasoning and tool
replay, carry large media without JSON base64, and make the point of external
effect auditable.

## Decision

`rsi-ai` is a separate product between composition and Agent orchestration.
`rsi-ai-protocol` is the only AI semantic vocabulary. It defines rich language
messages and stream grammar, three typed media operations, a separate live
Realtime state machine, normalized errors and usage, bounded provider-private
extensions, locator-free media descriptors, and binary chunk framing.

The standalone façade is an immutable exact-routing `Registry` with five typed
model handles. Provider authors implement capability-specific traits in
`rsi-ai-provider`; shared HTTP/SSE machinery and concrete providers stay below
that seam. Every call has a provider-I/O-free Prepare phase and a consuming
single-attempt Start phase. The redacted prepared snapshot freezes deployment,
protocol, endpoint fingerprint, config generation, credential source, request
digest, and retry facts. Unsupported settings fail during Prepare rather than
being silently dropped.

`rsi-ai-meta` maps the capabilities to five version-zero services. One service
stream pins one provider generation, and binary media flows under explicit
credit. The production wrapper workspace statically advertises OpenAI's five
capabilities, compatible Chat and DeepSeek language, and Xiaomi transcription
plus speech. Provider selection remains a composition binding; Agent requests
contain only a model identifier.

`rsi-agent` owns durable policy. Language retries require a committed retry
event and stop after visible output. Direct media and Realtime operations have
caller-owned identities and commit Reserved, Prepared, Started, and terminal
database states. Reservation precedes provider stream opening and input media
reads. Recovery records NotStarted or OutcomeUnknown and never replays an
effect. Media bytes live in an owner-only, quota-bounded SHA-256 CAS; durable
records contain references. Realtime frames are intentionally live-only.

OpenAI Responses background work uses the optional provider deferred-language
seam. Submission, poll, resume, and cancel are explicit single requests. A
durable checkpoint carries the frozen call snapshot, remote response id,
status, stream-created flag, monotonic sequence cursor, and bounded parser
state. Resumed streams expose normalized events and their post-event cursor as
one atomic batch; the SDK never hides polling or reconnection.

## Alternatives considered

A single generic `run` API was rejected because capability inputs, outputs,
stream grammars, and lifecycle differ materially. A public generic Provider
trait or transport registry was rejected because it would expose HTTP and
routing machinery to ordinary callers. Putting AI contracts in
`rsi-agent-protocol` was rejected because standalone use and non-Agent media
operations would then depend on Agent vocabulary.

Inline base64 persistence was rejected because it duplicates large sensitive
payloads across requests, events, and replay. Automatic adapter retries were
rejected because a stream may already have visible output or an uncertain
effect. Realtime frame-by-frame durability was rejected because synchronous
commit before every frame defeats the latency contract; a crash therefore
ends the live plane rather than reconnecting it.

## Consequences

The product has more packages and five service keys, but each public surface is
small and capability-specific. Concrete provider behavior is verified with
local servers, while native plugin and Agent paths have keyless conformance
gates. Credentials and raw media stay out of semantic JSON and Debug output.

The registry accepts exact unlisted model identifiers and has no automatic
provider fallback. Standalone calls are single-attempt. The Agent remains one
durable language turn per session, has no public language delta stream, and
does not resume Realtime sessions. Artifact retention is quota-bounded but
manual; automatic garbage collection requires a later ownership decision.
