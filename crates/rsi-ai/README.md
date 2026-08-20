# rsi-ai

`rsi-ai` is the provider-neutral AI integration product. It exposes separate,
strongly typed language, image generation, transcription, speech, and Realtime
capabilities. A caller selects an exact provider deployment and model, prepares
a validated one-shot call without provider I/O, persists the redacted snapshot
when durability matters, and then starts the external effect.

## Components

| Package | Responsibility |
|---|---|
| [`rsi-ai-protocol`](protocol/README.md) | Closed semantic requests, normalized events/results, stream grammars, media descriptors, and binary framing |
| [`rsi-ai-auth`](auth/README.md) | Redacted secrets and deterministic standalone credential resolution |
| [`rsi-ai-provider`](provider/README.md) | Capability-specific provider-author seams and prepared-call snapshots |
| [`rsi-ai`](core/README.md) | Exact-routing registry and the small standalone façade |
| [`rsi-ai-transport`](transport/README.md) | Shared bounded HTTP body and SSE machinery |
| [`rsi-ai-openai-compatible`](openai-compatible/README.md) | Generic Chat Completions language adapter |
| [`rsi-ai-deepseek`](deepseek/README.md) | DeepSeek language policy and reasoning replay |
| [`rsi-ai-openai`](openai/README.md) | OpenAI Responses, Images, transcription, speech, and Realtime adapters |
| [`rsi-ai-xiaomi`](xiaomi/README.md) | Xiaomi transcription and speech adapters |
| [`rsi-ai-meta`](meta/README.md) | Five generation-pinned `rsi-meta` services and plugin wrapper |
| [`rsi-ai-testkit`](testkit/README.md) | Deterministic adapters and media resolver for keyless tests |

The standalone plugin workspace is [`plugins/rsi-ai`](../../plugins/rsi-ai/README.md).
Request schemas are owned by [`schemas/rsi-ai`](../../schemas/rsi-ai/README.md).
`rsi-agent` consumes the five service contracts and owns durable agent history,
retry scheduling, artifact retention, and live-session policy.

## Contract

Provider routing is fixed when a `Registry` is built. `ModelRef` always names a
deployment and model exactly; there is no alias, fallback, or request-level
endpoint override. `prepare` validates request/provider compatibility and
freezes the route, config generation, credential source, request digest, and
retry facts. `start` consumes that prepared value and performs one provider
attempt. Convenience completion methods drain the same validated stream.

Deferred language work is explicit rather than a retry mode. A supporting
provider returns a persistable `DeferredLanguageCheckpoint`; each poll, stream
resume, or cancel method makes one request. Resumed output arrives as atomic
event/checkpoint batches so callers can commit progress without guessing
whether an SSE cursor covers already-observed semantic events.

Media JSON contains only bounded descriptors. Bytes cross a `MediaResolver` at
Start time or bounded binary frames in the plugin protocol. Language and media
assemblers reject malformed sequence, duplicate terminal/usage data, oversized
output, and EOF without a terminal event. Realtime is a separate live,
non-replayable plane.

See [architecture](docs/architecture.md), [security](docs/security.md), and
[testing](docs/testing.md).
