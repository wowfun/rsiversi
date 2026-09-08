# rsi-api-uds-client

This ordinary native connection plugin supplies ApiClient over a same-UID Unix
socket. Configuration selects an absolute socket path, EndpointId, HostEpoch and
opaque LocalCompatibilityKey. Socket paths must also pass the host standard
library Unix address constructor, including its native length and NUL checks. The product owns their selection and build/launch
policy. This package imports no Service Host process owner or domain implementation.

Each exchange verifies the actual peer UID before HTTP transmission and sends
one HTTP/1 request with the local compatibility fence. It reuses shared connection
negotiation, admission, finite/binary/SSE decoding and uncertain mutation semantics.
The response source directly owns and polls the Hyper connection; there is no
detached socket driver. Dropping a read or retiring the connection releases that
socket, while a mutation already admitted at the server retains its server owner.
Local client shutdown never stops the independent deployment.
The response owner closes its socket by dropping the connection after HTTP
completion. It does not issue a redundant write shutdown after the peer has
closed; that system call can report NotConnected on Unix even after a complete
valid response. HTTP framing and body failures still fail the exchange.

The local listener closes HTTP/1 after one exchange. Pipelined extra requests
cannot dispatch a second operation and do not roll back an admitted first
mutation. They are no longer interpreted as private-protocol trailing frames.

Tests exercise real isolated Unix listeners, malformed responses, generation and
credential fences, cancellation and ordinary plugin retirement. Native platforms
without Unix sockets do not expose this adapter. Browser closures exclude it.
