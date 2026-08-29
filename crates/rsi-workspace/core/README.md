# rsi-workspace

This package provides the Workspace registry contract and ordinary plugin. One
configured storage-domain backend persists a versioned record per Workspace,
including its immutable insertion order. Mutations serialize, publish only the
affected domain record durably, and then update the live snapshot. Readers keep
observing the previous committed snapshot while durable I/O is in flight. Once
a mutation acquires the commit slot, a service-owned task completes durability
and live publication even if the requesting future is dropped.

IDs are SHA-256 of the canonical UTF-8 physical path, making repeated
get-or-create idempotent on one host. Non-UTF-8 paths are rejected rather than
silently collapsed.
