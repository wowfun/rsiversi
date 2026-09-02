# rsi-ai security boundary

Provider plugins execute with the Host process authority. This family does not
provide a sandbox.

Requests, Profile configuration, provider bodies, SSE events, extension JSON,
and media are untrusted at their owning boundary. Closed DTOs reject unknown
fields; semantic constructors enforce identifier, count, byte, recursive JSON,
role, and media relationships. HTTP success and error bodies have independent
finite bounds. Durable provider-private state additionally validates its exact
namespace-local format version and internal key grammar before restoration.
Provider errors retain a bounded safe summary, phase, dispatch status,
and retry hints rather than raw response bodies.

Serde validation is not a framing boundary: a decoder may allocate a string
before semantic validation. Transports, Profile parsing, and durable readers
must cap bytes before invoking it. Typed provider-control projections declare
per-field decoded string bounds that the transport enforces before serde
retention. Incremental media extraction uses separate
body, envelope, item, nesting, and decoded-output limits. Consumers publish an
operation only after its terminal validation succeeds.

Secrets are owned by `rsi-credentials-protocol::SecretValue`, zeroized when the
last owner drops, and formatted only as redacted. Prepared snapshots retain a
credential source identity, never a secret. Authorization headers are built
through temporary zeroizing text, marked sensitive, and consumed by the true
external HTTP request; copies retained by `http` or `reqwest` do not promise
zeroization and must not be logged or retained after the attempt. Provider
configuration cannot override credentials or endpoints per request.

Media requests contain only bounded descriptors. Routers reject more than 256
MiB of unique declared media during Prepare. The Media resolver reads bytes at
Start only after atomically acquiring the prepared call's complete byte weight,
then verifies length and digest while retaining that admission with cached
bytes. Language requests also enforce total media occurrences and declared raw
bytes before resolver I/O. Identical complete
descriptors may be coalesced only inside one prepared call. Provider adapters
must project buffered and streaming request framing against the transport body
limit before dispatch; multipart framing bytes count toward the same limit.
Multipart boundaries are digest-derived 96-bit values and are not synchronously
searched across hundreds of MiB of already authenticated bodies.

Dropping or cancelling the final waiter for a queued prepared-call media
admission removes its semaphore position. A successful complete-weight permit
is retained only with that prepared call's resolved media and is released when
the cache or context drops.

Retry safety is an orchestration rule, not a transport convenience. Dispatched
or dispatch-uncertain effects are not repeated automatically even if their
error kind is otherwise retryable.

SSE remains bounded per provider wire contract. OpenAI Responses admits a
larger finite frame because documented terminal events may include the complete
response; Chat-compatible providers retain the smaller default delta-frame
bound. Each transport item has a separate 256 KiB ceiling, so an untrusted
stream cannot retain a large multi-event backing allocation across consumer
yields while accounting only for the current event. Larger frames are assembled
from multiple bounded items. A decoder admits retained frame bytes incrementally from a fixed
process-wide budget. Each unfinished frame declares its finite maximum, and
growth is granted only while the complete set of unfinished claims remains in
a safe state where every frame can finish in some order, releasing its
admission for the remaining frames.
Completed `data` values retain only their actual weight until the consumer
drops them. This prevents both unbounded retention and partial-weight deadlock
without serializing streams merely because their declared maxima overlap.
Semantic output and event-count bounds still apply after decoding.
