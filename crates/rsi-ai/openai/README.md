# rsi-ai-openai

The model-discovery helper performs one bounded `GET /v1/models` against an
explicit endpoint and credential snapshot. Its shared OpenAI-list parser accepts
candidate identifiers and optional capacity metadata; this neither creates a
Language route nor proves a model supports Responses or Tools.
Discovery, Responses and Images accept either an API root or its `/v1` base;
the version prefix is appended only once. Custom gateway prefixes are preserved.
The shared list parser rejects responses declaring further pages (`has_more`
or a nonempty `next`/`next_cursor`) rather than presenting an incomplete list.

This package implements the official OpenAI Responses and Images adapters as
one ordinary deployment plugin. HTTP seams are injectable so default tests use
local deterministic servers rather than live credentials. The plugin requires
the Base credential contract and the enabled Language/Image registrar
contracts; it publishes no service of its own.

Responses also supports explicit background submission, one-shot poll/cancel,
and resumable SSE retrieval. The SDK performs no polling loop or hidden retry;
each normalized batch includes the checkpoint that a durable caller must
commit atomically with its events. The adapter validates the batch against a
candidate checkpoint and replaces its shared checkpoint only after that batch
is accepted. Terminal event kinds are authoritative, and
an embedded status must agree with the event kind. A max-output-token
incompletion is a successful `MaxTokens` terminal in both the event batch and
checkpoint; other incomplete responses are failures. A terminal status from
poll does not prevent resuming historical output; only a durably checkpointed
terminal stream event closes the event cursor. Poll and cancel accept a
bounded complete response object large enough for the maximum Language output,
but stream it through a typed `id`/`status` projection rather than retaining
ignored output content. Deferred parser state uses exact version 1 with
lowercase SHA-256 open-block keys; restore rejects older versions and malformed
keys rather than attempting an ambiguous migration. The parser caches its
immutable extension snapshot and rebuilds it only when the open-block map,
next index, or tool-seen bit changes. Ordinary text and tool-argument deltas
therefore clone shared state without reserializing it; checkpoint JSON remains
byte-for-byte unchanged.

Provider response identities become replay extensions only through the bounded
extension constructor. An identity that cannot fit that durable contract is a
typed output-validation failure; untrusted terminal events never reach a panic.

Typed Responses endpoint options select its path, instruction-role translation
and state mode. An endpoint owner may declare one disabled-reasoning alias,
serialized as Responses `none` while preserving the semantic request and
profile identity. The default uses OpenAI response identities and deferred
operations. Stateless mode uses inline plain reasoning, emits no response-id
replay extension, rejects such extensions on input and rejects deferred work.
These options do not change Images URLs or provider-neutral request identity.

Model-list helpers return typed `AiError` categories for request, transport,
HTTP status and response failures. Their diagnostics exclude raw upstream bodies
and transport text; discovery reads do not schedule provider retries.
