# rsi-workspace

`rsi-workspace` is an ordinary plugin for durable host-local workspace
registrations. It requires the non-session domain facility and Execution resolver, stores canonical
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
Domain version 4 retains mandatory execution coordinates and an allocation
high-water mark independently of live registrations, including when the registry
is empty. Older domains are rejected without mutation. Registrations and cursors
must be recreated in a separate new state directory; the old backend remains
untouched. The product owns its complete state-directory cutover.

Directory status and post-canonicalization validation reject an observed final
symlink. These are host-path checks, not an inode lease: filesystem changes after
validation remain possible, and effect providers must validate their own handles.
If a known registration is absent during durable deletion, the registry reports
Corrupt instead of claiming a successful synchronized deletion.

The [path library](path/README.md) owns the bounded cross-platform grammar for
host paths carried as data. It is independent of the registry, API and Runtime.

`WorkspaceIngress` binds one registry view to the authenticated `CallOrigin`.
Every exact read, registration, status and deletion admits that caller for the
record's execution location. Listing and complete order membership filter denied
locations before pagination or seed bounds. Read authorization does not require a
live SSH connection. Revocation denies new operations; an admitted bounded commit
retains its permit until settlement, including after its waiter disappears.

Registration takes an explicit location and an absolute target path. SSH
canonicalization and status use a single caller-bound Execution lease; no remote
path reaches the Service filesystem. Failure to contact the target is unavailable,
never evidence that a directory is missing. Native canonicalization stays with
Workspace's native filesystem boundary. Registrations contain no reusable grant.

Workspace's API plugins own their versioned request DTOs, operation bounds and
client proxy. The version-3 endpoint consumes trusted ingress and the API registrar; the client
publishes the same Workspace contract over a negotiated connection. Generic
transports do not classify Workspace operations. Transport failures preserve the
API error taxonomy, including authentication and uncertain mutation outcomes.
