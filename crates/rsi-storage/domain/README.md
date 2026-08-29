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
