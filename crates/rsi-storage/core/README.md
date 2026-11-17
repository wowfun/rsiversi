# rsi-storage

`StorageError::Closed` identifies a retired owner, distinct from an unavailable
backend registration. It does not require recovery of the durable generation.

Native directory publication creates missing private components on Unix and
Windows. Unix synchronizes the ancestor chain through the shared
[native filesystem helper](../../rsi-files/native-fs/README.md), including
pre-existing ancestors from an interrupted creation. Windows retains each newly created
directory and changed parent through no-follow NTFS handles and flushes only those
namespace changes; existing higher ancestors
remain unchanged and need no write permission. Sync failures prevent success.

`StorageError::Unsupported` reports a missing backend or platform capability
before admission, separately from malformed input and uncertain commit outcomes.
The domain API projects an unsupported backend into `ApiError::Unavailable`.

This package defines the bounded JSON KV backend contract and provides the
ordinary `rsi.storage` hub plugin. A backend registration belongs to the
registering plugin generation and disappears when its lease is dropped.

The hub performs exact-name routing only. It does not open files, choose a
default backend, retry failed writes, or expose mutable registry internals.

A backend opens an owned unit from a validated descriptor and returns its initial
bounded snapshot. The unit retains the descriptor and medium state; it does not
clone routing/layout declarations for each mutation. One backend reserves a unit
name until its final lease is released. Domain close drains its queue and drops
that unit; actual background I/O retains a lease through completion. JSON unit
documents therefore leave memory when the owning unit closes, without a backend
cache of historical domain names. Rust ownership supplies this lifetime rather
than a second asynchronous unit-close protocol.

A raw mutation batch contains at most 131,072 entries, twice the 65,536-record
capacity. This admits deleting every old address and inserting a full replacement
atomically, while bounding even missing-key deletions before address validation
or backend work. Domain owners apply the same rule at their smaller declared
capacity. Callers own input buffers until admission.

Backend operations acquire one owned slot before dispatching blocking work;
cancelling a caller after dispatch does not release that slot. Opening reserves
its unit name after acquiring the slot, so cancelled initialization can retry
after the actual reader releases its lease. Disposal closes
admission, drains accepted work, then withdraws registration. A registration guard
closes admission even if its cleanup callback or future is dropped; withdrawal
drops the registration outside the admission lock. The actual
blocking worker retains registration until it finishes. Old handles stay
unavailable. A replacement generation may open only after disposal completes.
The slot bounds dispatched blocking work, not caller-owned futures or payloads.
In-process callers bound their own outstanding requests; API owners apply their
admission quotas before invoking storage. Draining does not impose a syscall
termination deadline: releasing registration while an old worker can still write
would permit overlapping generations.

`KvBackend::ensure_available` is a local, non-I/O health check. `Io` means a
mutation is known not to have committed. `OutcomeUnknown` means commit cannot
be established; that backend generation is permanently fenced, including every
domain routed to it, and all later operations return `RecoveryRequired`.
Recovery recreates the backend, domain
facility and consumers from durable state; there is no automatic retry or reset.
Consumers retaining projections check health before admitting work or serving
cached state, including when the failed write belonged to another domain.
A [Domain authority](../domain/README.md) can additionally fence its own
publication task without fencing a healthy backend or its other domains.

Values have at most 64 levels, counting the root as level one, and at most
16 MiB of compact JSON. Encoding checks limits before growing its output;
existing out-of-contract durable values are corruption, without migration.
`EncodedValue` retains validated compact JSON with shared immutable ownership.
Typed producers encode directly through the bounded writer; durable readers
admit bounded bytes once, and owning Domain codecs decode directly from those
bytes. JSON syntax and depth belong to this storage boundary. Business schemas
and semantic constructor invariants belong to the Domain declaration. Safe-Rust
codecs must serialize values their own decoder accepts; storage does not rerun
the business decoder on every in-process write.
`RecordObjectSize` and `encoded_entry_bytes` supply exact compact record-object
accounting, including encoded keys, braces, colons and commas. Invalid restored
object geometry reports `Corrupt`; projection arithmetic errors report
`InvalidInput`. Restored metadata
uses bounded stored value lengths and must be checked against durable rows at
the backend boundary; decoding additionally proves that these lengths equal
compact JSON lengths. Domain specifications
and backend aggregate ceilings use this definition; version-envelope bytes belong
only to a medium's separate file limit.

For table/global units the logical accounting object uses the empty key for the
global and `table/key` for each table record. Table names exclude `/`; business
keys may contain it, so this projection is injective. Its exact encoded key,
value, brace and comma lengths determine the limits, independently of SQLite
columns or JSON envelope framing. Business owners retaining older logical
payload caps enforce those caps separately when a new layout adds framing.
