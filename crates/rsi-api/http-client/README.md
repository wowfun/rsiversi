# rsi-api-http-client

This native connection plugin publishes one negotiated `ApiClientContract` using
the shared Rust connection owner and API decoder. Its transport owns native HTTP,
credential headers, TLS and delivery classification. Configuration contains an explicit canonical origin,
expected EndpointId and Credentials reference; plaintext credentials are never
serialized. HTTPS validates normal server trust or an explicitly supplied bounded
PEM CA. Development HTTP requires explicit opt-in and a loopback origin. Redirects,
ambient proxies, decompression and HTTP connection pooling are disabled.
Authorization construction uses temporary zeroizing text and sensitive headers.
Copies held internally by the HTTP stack do not promise zeroization and are never
logged or serialized as configuration.

Negotiation verifies deployment identity, HostEpoch, wire version and a bounded
duplicate-free operation catalog before publication. Domain calls require their
exact advertised descriptor. The shared [connection owner](../client/README.md)
separately bounds input, receiving and completed-response retention. Local
admission uses device limits 4/4/8 without a queue. Finite receive capacity is
reserved before sending a request.

Client retirement fences new calls and cancels its local reads, streams and result
waiters. It never stops the remote Host. A network failure after possible mutation
dispatch, malformed result or changed generation reports OutcomeUnknown. A proven
connect failure and a validated remote rejection retain their known failure.
The client never retries a mutation. Connection negotiation has a 15-second
absolute deadline, finite exchanges one minute, and stream completion five seconds
after an explicit end; idle subscriptions have no polling deadline.

Tests use real isolated API listeners and malformed-response fixtures, with no
user credentials, keyring or live service access. Native Windows/macOS behavior
requires execution on those platforms.
