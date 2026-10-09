# rsi-storage-domain

This package provides the ordinary domain-form plugin. Consumers open one
bounded JSON record domain with an exact backend route and schema version.
Writes are serialized for that domain, reach the selected backend first, and
only then update the observable snapshot. After a write acquires the domain
commit slot, a domain-owned task completes both steps even if its requesting
future is dropped; cancellation while waiting for the slot starts no durable
work. Each specification bounds both record count and the exact compact JSON
bytes of the complete record object; loaded state and every projected write are
checked against both bounds before publication.

The facility has no fallback route and performs no schema migration. Opening
the same domain with a different specification fails loud.

Opening reserves domain authority before loading. Concurrent opens share one
initialization; cancelled or failed initialization can be retried. The complete
specification stays fixed while opening, held by a consumer, or retained by an
accepted mutation. A mutation retains authority through snapshot publication.
Dead authorities remove their own registry entry by identity, without retaining
historical domain names. The loaded backend generation never changes in place.

Snapshots return a result and check backend health, including after waiting for
the domain slot. Cached opens and no-op deletes also reject unavailable storage.
Both mutations check health before validating their arguments. Each retained
value carries its measured entry size; load validates and measures it once, and
acknowledged mutations update that size together with the value.

The [core record-object accounting contract](../core/README.md) owns
`RecordObjectSize` and `encoded_entry_bytes`. Consumers planning
retention use the same helpers with cached entry sizes; a projection is a pure
calculation, not a reservation or a durable mutation. The caller supplies exact
compact value lengths and the measured size of the entry being replaced or
removed. Domain admission remains authoritative for its specification's bounds.

A lost domain commit task reports `OutcomeUnknown` and fences that domain authority
before releasing its commit slot, including when its backend remains healthy.
The shared `storage_error` API projection preserves unknown outcomes, maps known
pre-commit I/O failures and unavailable generations to `Unavailable`, and maps
input rejections to `Invalid`. Corruption and duplicate-backend diagnostics remain
`Backend` errors. The API dispatcher
still treats an untyped `Backend` mutation failure conservatively as unknown.
This adapter consumes only the API protocol; the API foundation does not import
Storage implementations.
