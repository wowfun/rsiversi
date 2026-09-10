# rsi-workspace

`rsi-workspace` is an ordinary plugin for durable host-local workspace
registrations. It requires the non-session domain facility, stores canonical
physical absolute paths and stable order, and provides a Local registry. Each
workspace is one bounded domain record carrying its immutable insertion order;
mutations never rewrite unrelated registrations.

Workspace state is not Agent context, a sandbox grant, or directory ownership.
Deleting a registration removes only domain records; user directories, files,
Sessions, and Agent facts are never removed. Missing directories are reported
by status and do not mutate the registry.

`rsi-workspace-protocol` owns the transport-independent registry contract and
validated WorkspaceId. The native plugin owns canonicalization, durable records
and directory status. An exact `get` reads one registration without filesystem
access. Listing takes a bounded page size and a stable insertion-order cursor;
deleting the cursor's registration does not invalidate continuation. Returned
paths identify the server's directory and confer no client filesystem authority.
Domain version 3 retains an allocation high-water mark independently of live
registrations, including when the registry is empty. Older domain versions are
rejected: surviving records cannot reconstruct deleted allocation history.
Path-derived identities are unchanged.

For an old registry, stop all owners and clients, preserve a byte-for-byte backup
of its configured storage backend, and export the `rsi.workspace` registrations
with the old binary or from that backup. There is no automatic in-place upgrade:
changing the version or inferring a high-water mark from surviving rows would
reuse previously issued cursors. To deliberately start a new registry, discard
all old Workspace listing cursors, remove only the `rsi.workspace` domain from an
offline copy of the JSON backend's `domains` object, and preserve every other
domain and the original backup. Publish that copy only while the backend is
stopped, then register the exported, still-existing absolute directories through
Workspace. Their path-derived IDs remain stable; insertion orders are newly
allocated. Sessions and user directories need no deletion. Operators needing
continuity of old cursors must retain the old registry/version.

Directory status and post-canonicalization validation reject an observed final
symlink. These are host-path checks, not an inode lease: filesystem changes after
validation remain possible, and effect providers must validate their own handles.
If a known registration is absent during durable deletion, the registry reports
Corrupt instead of claiming a successful synchronized deletion.

The [path library](path/README.md) owns the bounded cross-platform grammar for
host paths carried as data. It is independent of the registry, API and Runtime.

Workspace's API plugins own their versioned request DTOs, operation bounds and
client proxy. The endpoint consumes the registry and API registrar; the client
publishes the same Workspace contract over a negotiated connection. Generic
transports do not classify Workspace operations. Transport failures preserve the
API error taxonomy, including authentication and uncertain mutation outcomes.
