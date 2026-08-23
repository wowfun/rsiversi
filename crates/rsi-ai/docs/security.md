# rsi-ai security boundary

Provider adapters execute with the caller's process authority. The standalone
SDK does not provide process isolation or a sandbox.

Requests, provider bodies, SSE events, WebSocket messages, extension JSON, and
media are untrusted at their owning boundary. Closed DTOs reject unknown fields;
semantic constructors enforce role, identifier, count, byte, JSON-depth, and
media-kind relationships; assemblers enforce temporal grammar and total output
bounds. Successful HTTP bodies and error bodies are collected with explicit
limits. Provider errors expose a bounded safe summary, phase, dispatch status,
and retry hints rather than raw response bodies.

Semantic `Deserialize` implementations validate each completed typed value,
but generic Serde deserialization is not a byte-framing boundary: it may
materialize a string before its semantic byte limit is checked. Transports and
durable readers must cap the input bytes before invoking Serde. This package's
HTTP, SSE, WebSocket, and binary ingress paths do so at their owning boundary.

Large media JSON responses are incrementally extracted under independent total
body, normalized-envelope, item, nesting, and decoded-media limits. OpenAI
Images retains at most one bounded image item plus its small normalized
envelope; Xiaomi speech retains one bounded base64 chunk plus its normalized
envelope. Successfully decoded chunks or items may precede a later terminal
validation error, so callers must publish only the completed operation result.
Limits remain per call, and deployments must include configured concurrency in
their process memory budget.

`SecretValue` is zeroized when its last owner is dropped, always formats as
redacted, and deliberately exposes no equality operation. A prepared snapshot identifies only the selected credential source.
Standalone resolution precedence is explicit: in-memory, persistent store,
then the environment captured by the builder. Secrets never enter snapshots or
Debug output.

Media requests contain locator-free descriptors. A resolver reads bytes only at
Start and verifies length and SHA-256 before translation. Provider URL fetching
is not part of the protocol.

Realtime is intentionally non-replayable. A dropped session does not reconnect
or resend frames.
