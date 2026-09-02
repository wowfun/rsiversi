# rsi-ai-openai

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
