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

Media JSON responses are currently whole-body operations. One OpenAI image
call may transiently retain at most 448 MiB of JSON/base64 before decoded image
buffers and output chunks are published; one Xiaomi audio call may retain at
most 180 MiB before decoding. Those limits derive from the semantic maximum of
ten 32 MiB images and one 128 MiB audio body plus base64 and envelope overhead.
They are per call, so callers must include concurrency when setting a process
memory budget.

`SecretValue` is zeroized when its last owner is dropped and always formats as
redacted. A prepared snapshot identifies only the selected credential source.
Standalone resolution precedence is explicit, in-memory, persistent store,
then the environment captured by the builder. Plugin wrappers receive secrets
only from fields marked `x-rsi-meta-secret`; they do not consult the process
environment or OS keyring and never serialize a secret into config snapshots,
events, or Debug output.

Media requests contain locator-free descriptors. A resolver reads bytes only at
Start and verifies length and SHA-256 before translation. Plugin binary frames
are bounded, sequenced, credited, and reassembled against the same descriptor.
Provider URL fetching is not part of the protocol. `rsi-agent` stores media in
an owner-only, quota-bounded CAS and re-verifies every read.

Realtime is intentionally non-replayable. Sending live audio before a complete
recording can be durably committed is an explicit latency tradeoff. A crash or
dropped session does not reconnect or resend frames; recovery records an
uncertain started operation instead.
