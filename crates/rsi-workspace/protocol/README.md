# rsi-workspace-protocol

Registrations may carry a custom display name and their registration time in
Unix milliseconds. Absent metadata means the path supplies the name and the
creation time is unknown; readers never synthesize a historical timestamp.
Names are trimmed, nonempty UTF-8 strings of at most 256 bytes without control
characters. Rename replaces only the display name, retaining identity,
coordinates, creation time and insertion order. New native registrations record
their creation time. Rename and deletion require the same location admission as
registration and retain admitted commits through durable publication.
Authenticated registration can return the record and whether this call
created it. The registry decides that result under its existing commit lock.

Registrations carry mandatory validated execution coordinates. `WorkspaceId`
identifies one registration lifetime; it is opaque and independent of the path.
Deleting and re-registering the same directory creates a new identity, so an old
identity cannot address the replacement. Exact `get` reads by registration ID;
`find` reads by `WorkspaceKey` and returns an optional current registration.
Both operations authorize the record's location without filesystem access.

`WorkspaceKey` is derived only by this owner from validated execution
coordinates. Local identity uses its normalized absolute path bytes; SSH identity
uses a distinct `ssh` domain prefix, stable target identity and target path. Since
a Local path is absolute, it cannot occupy the SSH-prefixed input namespace.
No consumer hashes paths or interprets an SSH path on the Service filesystem.
History and review scopes use this coordinate key; they do not manufacture a
registration identity. Neither kind of identity grants filesystem access.

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

The registry also owns a shared display order, independent of insertion-order
pagination and browser-local view preferences. New registrations lead this order.
`insert_before` moves one visible registration before a visible anchor, or to the
end when the anchor is absent, and returns the committed visible order. Both
identities must exist and admit the caller. Self-anchored and already-positioned
moves perform no durable write and publish no revision. Concurrent moves apply to
the latest committed order. Accepted moves survive loss of their waiter and
retirement drains them. Rename, filesystem activity and Session activity never
change display order. Deleting and recreating the same path creates a new position.
The display-order response contains only unique identities, bounded by the
registry's 16,384-registration capacity; it copies no Workspace records or history.

`WorkspaceKey::from_canonical_path` is an explicitly Local convenience for callers
that already own a native canonical directory. Cross-location consumers derive
the coordinate key through `WorkspaceKey::from_coordinates`.

The native ingress can expose a process-local directory revision. Publishing, renaming, deleting or moving a registration changes the revision. Reusing an existing record or an unchanged display position leaves it unchanged.
