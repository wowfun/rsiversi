# rsi-api-auth

The native device authentication plugin publishes separate verification and local
administration capabilities. It requires an explicit Storage backend and an
EndpointId supplied by the deployment owner. One bounded domain record retains
the deployment identity and up to 64 device token verifiers; labels are at most
128 UTF-8 bytes. Loaded records reject unknown fields, duplicate identities or
hashes, invalid labels, and a mismatched deployment identity.

Mutations serialize through one commit owner. Once admitted to that owner, the
explicit executor completes durable publication and updates authentication state
even if the requesting waiter disappears. Revocation cancels live authentication
leases only after the write succeeds. Shutdown fences new calls, drains a commit
already in progress and revokes escaped read/stream leases. The provider never
logs, serializes or persists plaintext tokens. Its public embedding constructor
requires the same exclusive domain writer guaranteed by the deployment lease.

Run `cargo test -p rsi-api-auth --all-targets` for persistence, failure and
revocation scenarios with an injected isolated Storage domain. Native credentials,
HTTP authentication, TLS and browser cookies have separate adapter verification.
