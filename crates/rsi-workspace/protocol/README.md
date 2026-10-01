# rsi-workspace-protocol

Registrations carry mandatory validated execution coordinates. Their identity
includes the owning machine and canonical path; decoding and validation never
probe the receiving client's filesystem. A path alone cannot identify an SSH
workspace. Consumers compare complete coordinates before using a registration.

Workspace identity is derived only by this owner from validated execution
coordinates. Local identity uses its normalized absolute path bytes; SSH identity
uses a distinct `ssh` domain prefix, stable target identity and target path. Since
a Local path is absolute, it cannot occupy the SSH-prefixed input namespace.
No consumer hashes paths or interprets an SSH path on the Service filesystem.

This library owns validated Workspace identities, exact registration reads and
bounded insertion-order pages. It contains no native filesystem or storage
implementation. A returned path names a directory on the registry's host; it is
not authority to open a directory on the caller's device. Unknown identities
are typed errors. Listing cursors remain usable after deleting a registration.

Workspace paths are UTF-8 and bounded to 16 KiB before registration or durable
loading. Adapter responses validate record identity/path bounds, unique page
identities, requested count and advancing continuation before retention. A path
in a remote response remains opaque host data; client-native path syntax does
not validate another host's filesystem.

The separate order seed contains complete membership from one registry snapshot,
sorted by identity, with at most 1,024 records and 128 KiB of encoded JSON. An
over-limit registry returns `TooLarge`, never a partial seed. Device ordering must
preserve its saved order and pause reconciliation in that case; ordinary pages
remain available. Page completion is not evidence of complete membership.
Construction and validation measure the same encoded envelope; validation borrows
the supplied records without constructing a second membership snapshot.

Remote proxies preserve API failures in `WorkspaceError::Api`, including unknown
mutation outcomes. A failed reply is not proof that registration or deletion did
not happen. The caller reconciles through exact registry identity and reads; the
proxy does not replay mutations or reinterpret API failures as storage failures.

`WorkspaceId::from_canonical_path` is an explicitly Local convenience for callers
that already own a native canonical directory. Cross-location consumers derive
identity from complete coordinates through `WorkspaceId::from_coordinates`.
