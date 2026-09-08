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

IDs are SHA-256 of the canonical UTF-8 physical path, making repeated
get-or-create idempotent on one host. Non-UTF-8 paths are rejected rather than
silently collapsed.
