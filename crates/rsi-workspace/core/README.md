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

Domain version 4 stores mandatory execution coordinates and the independent
allocation high-water mark. Workspace protocol owns identity derivation from
those coordinates. Local get-or-create canonicalizes only the Service filesystem;
it never interprets a remote registration's path as Local. Non-UTF-8 paths are
rejected rather than silently collapsed. Older domain records are rejected.

Cache availability follows [Storage generation health and recovery](../../rsi-storage/core/README.md).

Storage failures retain the shared [Domain API projection](../../rsi-storage/domain/README.md)
in `WorkspaceError::Api`, including known pre-commit rejection, unknown commit
outcome and an unavailable generation. They are not reduced to diagnostic text.
