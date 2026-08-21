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
across providers.

Text-only JSON requests remain one buffered body with a `Content-Length`.
Requests containing binary media stream base64 from retained bytes without an
encoded full-body copy. Structured JSON Pointer slots identify insertions;
caller strings are never interpreted as replacement markers. The raw base64
chunk size is compile-time constrained to a multiple of three.

The streaming response extractor owns JSON string/key escaping, nesting, total
body, retained envelope, and extracted-item bounds. It replaces an extracted
string with `""` or each extracted array item with `null`, then returns that
bounded normalized envelope for the provider's typed semantic validation. Each
object-array item is syntactically validated before it is emitted; later
envelope validation remains terminal for the whole operation.

The production client performs one request with redirects disabled, a 10-second
connect timeout, and a five-minute whole-request timeout. Callers that need
different finite deadlines can construct it with `ReqwestTransport::with_timeouts`.
Cancellation is observed both before response headers and while the response
body is being consumed.
