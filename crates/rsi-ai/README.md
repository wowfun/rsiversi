# rsi-ai

`rsi-ai` owns the provider-neutral Language and Image capabilities. Each
runtime capability is an ordinary `rsi-meta` Local service: routers and concrete
deployments are independent plugins whose availability follows their supplying
Fiber generation. There is no AI-specific Meta bridge or credential subsystem;
credentials and media come from the Base contracts that own them.

The protocol package owns validated requests, normalized events, stream
grammars, exact `ModelRef` routing, and redacted prepared-call snapshots. The
Language and Image router packages publish their Local contracts. Provider
packages register exact deployments with those routers and remain unaware of
Agent session semantics. Shared transport code performs one bounded HTTP/SSE
attempt and never schedules retries. Deterministic test support is keyless.

The two request schemas are owned by
[`schemas/rsi-ai`](../../schemas/rsi-ai/README.md).

## Contract

Each router generation owns an exact deployment table populated by
generation-bound provider leases. `ModelRef` always names a deployment and
model exactly; there is no alias, fallback, or request-level endpoint override.
`prepare` validates request/provider compatibility and
freezes the route, config generation, credential source, request digest, and
retry facts. `start` consumes that prepared value and performs one provider
attempt. Callers validate terminal success by draining the normalized stream
through the matching assembler.

Deferred Language work remains an explicit provider API rather than a retry
mode. It is reachable through the same generation-pinned Language Local
service as direct calls; prepare/restore and each submit, poll, resume, or
cancel operation preserve the frozen route and perform at most one request.

Media JSON contains only bounded descriptors. Prepare rejects a request whose
unique declared media bytes exceed the 256 MiB process resident budget. Bytes
cross a `MediaResolver` at Start time only after the prepared call acquires its
complete request weight atomically. Language and Image assemblers reject malformed order, duplicate
terminal data, zero-progress deltas, excessive event counts, oversized output,
and EOF without a terminal event. Durable Image consumers may move each closed
body out of the assembler before accepting the next output.

See [architecture](docs/architecture.md), [security](docs/security.md), and
[testing](docs/testing.md).
