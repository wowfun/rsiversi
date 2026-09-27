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

Local administration can rotate a managed device by a bounded stable slot key.
The slot is persisted with the verifier, so rotation preserves DeviceId across
restarts and cancels prior credential leases only after publication. It shares
the ordinary device capacity and commit owner. Explicit device revocation removes the slot; a
later explicit local rotation creates a new identity. Labels do not identify
managed slots. No managed-device operation is exported to remote callers.

Local rollback can retire an exact issued credential. The comparison and publication
share the registry commit owner; a later rotation of the same DeviceId survives
cleanup of an older failed launch. This operation is not exported remotely.

Failed-launch cleanup retires a matching managed credential without deleting its
slot or DeviceId. A retired verifier cannot authenticate; retirement survives
restart and the next explicit rotation reuses the same principal. Exact cleanup
of an unmanaged credential still deletes its record. Explicit `revoke(id)` deletes
either kind, including a retired managed slot. Failure to persist retirement leaves
the previous credential and leases intact. Retired managed slots still count toward
the 64-device bound and remain visible to local administration.

Local managed-slot inspection returns only its non-secret record. Conditional
rotation compares the expected principal (or expected absence) under the same
commit lock as publication. A changed slot fails before token replacement; readers
and failed grants never revoke the existing credential. These operations are not
registered on the remote device API.
