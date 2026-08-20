# rsi-ai-transport

This internal support package provides injectable bounded HTTP transport,
successful/error body collection, and incremental SSE decoding shared by
concrete providers. Provider semantic translation remains in the owning
provider package.

The production client performs one request with redirects disabled, a 10-second
connect timeout, and a five-minute whole-request timeout. Callers that need
different finite deadlines can construct it with `ReqwestTransport::with_timeouts`.
Cancellation is observed both before response headers and while the response
body is being consumed.
