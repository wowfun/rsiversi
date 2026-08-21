# rsi-ai security boundary

Provider plugins are trusted native code in the `rsi-meta` host process. Service
capabilities control cooperative routing; they are not process isolation or a
sandbox.

Requests, provider bodies, SSE events, WebSocket messages, extension JSON, and
media are untrusted at their owning boundary. Closed DTOs reject unknown fields;
semantic constructors enforce role, identifier, count, byte, JSON-depth, and
media-kind relationships; assemblers enforce temporal grammar and total output
bounds. Successful HTTP bodies and error bodies are collected with explicit
limits. Provider errors expose a bounded safe summary, phase, dispatch status,
and retry hints rather than raw response bodies.

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
Standalone resolution precedence is explicit, in-memory, persistent store,
then the environment captured by the builder. Plugin wrappers receive secrets
only from fields marked `x-rsi-meta-secret`; they do not consult the process
environment or OS keyring and never serialize a secret into config snapshots,
events, or Debug output.

Media requests contain locator-free descriptors. A resolver reads bytes only at
Start and verifies length and SHA-256 before translation. Plugin binary frames
are bounded, sequenced, credited, and reassembled against the same descriptor.
Realtime input credit is returned only when the provider task dequeues the
corresponding command, bounding a stalled bridge without blocking the plugin
callback or converting ordinary pressure into stream failure.
Provider URL fetching is not part of the protocol. `rsi-agent` stores media in
an owner-only, quota-bounded CAS and re-verifies every read.

Realtime is intentionally non-replayable. Sending live audio before a complete
recording can be durably committed is an explicit latency tradeoff. A crash or
dropped session does not reconnect or resend frames; recovery records an
uncertain started operation instead.
