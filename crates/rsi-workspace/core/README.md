# rsi-workspace

This package provides the native Workspace registry plugin over the shared protocol. One
configured storage-domain backend persists a versioned record per Workspace,
including its immutable insertion order. A separate bounded allocation record
reserves each order durably before its registration is written. Deletion never
removes this high-water mark. Failed creation may leave a gap; orders cannot be
reused after restart. Mutations serialize and update the live snapshot after
durable registration publication. Readers keep
observing the previous committed snapshot while durable I/O is in flight. Once
a mutation acquires the commit slot, a service-owned task completes durability
and live publication even if the requesting future is dropped.
Retirement fences new access and drains the commit slot before withdrawing either
registry registration. Accepted commits run on the owning Execution runtime.
Complete order membership streams visible records into its count and byte bound;
it does not materialize the entire registry before detecting overflow.

The allocation record also retains the Host display order as creation positions,
not copied Workspace records. On opening, the registry drops positions whose
registration was deleted and prepends newer registrations. Deletion removes the
live position immediately; the next allocation or changed move prunes retired
positions from the durable list. This list stays within the registration bound
and cannot resurrect a deleted position when its path is registered again.
One allocation-record commit publishes a changed order. A failed commit keeps the
previous live order; unknown outcomes use the Storage generation's existing fence.
Insertion-order pages and their durable cursors remain independent of this order.

Domain version 4 stores mandatory execution coordinates and the independent
allocation high-water mark. A new registration receives an opaque random identity
in its existing record; coordinate keys form a separate in-memory lookup index. Local get-or-create canonicalizes only the Service filesystem;
it never interprets a remote registration's path as Local. Non-UTF-8 paths are
rejected rather than silently collapsed. Older domain records are rejected.
Registration checks the coordinate index before and after acquiring the commit
slot. Both identity and coordinate indexes are reconstructed from the same durable
records; neither creates a second stored mapping.

Cache availability follows [Storage generation health and recovery](../../rsi-storage/core/README.md).

Storage failures retain the shared [Domain API projection](../../rsi-storage/domain/README.md)
in `WorkspaceError::Api`, including known pre-commit rejection, unknown commit
outcome and an unavailable generation. They are not reduced to diagnostic text.
