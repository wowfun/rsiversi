# rsi-ai-transport

This internal support package provides injectable bounded HTTP transport,
successful/error body collection, incremental SSE decoding, streamed JSON
request encoding, and bounded extraction of large JSON string fields or object
array items. Provider semantic translation remains in the owning provider
package.

Failures while delivering response bytes remain transport errors. Once a
provider returned a successful response, exceeding a configured body, JSON
envelope, item, or nesting bound is output validation; malformed JSON shape or
encoding is a protocol error. Shared mappers keep that distinction consistent
across providers. The shared OpenAI-compatible context-limit classifier changes
only the provider-neutral kind and preserves every already-validated status,
provider code, phase, dispatch fact, retry hint, request ID, and safe summary.

SSE decoding requires the provider to select a finite frame ceiling within the
transport's absolute bound. Delta-oriented protocols use the 256 KiB default.
OpenAI Responses uses a larger bounded ceiling because a terminal event may
carry the complete response; normalized event-count and output-byte limits
remain separate semantic gates. The decoder acquires the selected ceiling as
one process-wide byte weight before consuming the stream; waiting decoders hold
no partial admission.

Text-only JSON requests remain one buffered body with a `Content-Length`.
Requests containing binary media stream base64 from retained bytes without an
encoded full-body copy. Structured JSON Pointer slots identify insertions;
caller strings are never interpreted as replacement markers. The builder
rejects more than 256 replacements and computes the exact projected encoded
length before returning any body; concrete providers cap that projection at
384 MiB. Marker discovery and location each traverse the template once. The raw
base64 chunk size is compile-time constrained to a multiple of three.

The streaming response extractor owns JSON string/key escaping, nesting, total
body, retained envelope, and extracted-item bounds. It replaces an extracted
string with `""` or each extracted array item with `null`, then returns that
bounded normalized envelope for the provider's typed semantic validation. Each
object-array item is syntactically validated before it is emitted; later
envelope validation remains terminal for the whole operation. Slice admission
scans until the next event without buffering later items, avoiding a public
per-byte call on very large bodies.

Small typed control projections are deserialized from a bounded body stream
without retaining ignored provider fields. Every selected top-level string has
a declared decoded-byte bound that is enforced before `serde_json` can retain
the field in its parser scratch space. Projection parsing has a fixed
process-wide admission bound, and caller cancellation disconnects the parser
rather than leaving a queued background task.

The production client performs one request with redirects disabled, a 10-second
connect timeout, and a five-minute whole-request timeout. Callers that need
different finite deadlines can construct it with `ReqwestTransport::with_timeouts`.
Cancellation is observed both before response headers and while the response
body is being consumed. Bearer headers are constructed through temporary
zeroizing storage and marked sensitive before entering the request header map.
The request map and HTTP client necessarily own ordinary header bytes while the
attempt is live; they do not provide a zeroization guarantee and must not be
used as a credential cache.
