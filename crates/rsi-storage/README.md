# rsi-storage

`rsi-storage` is the non-session domain-storage family. It is deliberately not
the Agent session store and is not a generic persistence escape hatch for
Settings, Credentials, or Media.

The [`rsi-storage`](core/README.md) plugin owns a process-local backend hub.
Backend plugins register exact names with generation-owned leases. The
[`rsi-storage-domain`](domain/README.md) plugin opens bounded JSON record
domains with typed tables, a global slot and explicit record and aggregate-byte ceilings, serializes writes per
domain, makes a backend write durable first, and only then publishes the new
in-memory snapshot. [`rsi-storage-json`](json/README.md)
and [`rsi-storage-sqlite`](sqlite/README.md) are explicit local backend plugins.
Every backend also enforces the family-wide 65,536-record ceiling at raw commit:
updating an existing key remains valid at the ceiling, while inserting another
key fails before commit.

Domain and SQLite use the [core record-object accounting contract](core/README.md)
for aggregate limits. SQLite raw writes reject projected growth beyond 256 MiB
before commit, and complete loads reject that ceiling before decoding the body
that would cross it. JSON also bounds each complete serialized physical document to at most 64 MiB.
SQLite activates only its exact
STRICT schema, constraints, indexes, foreign key, and schema version; an
existing lookalike layout is corruption, not an implicit migration source.

There is no implicit backend, path, format migration, cross-process merge, or
fallback routing. Duplicate backend names, missing routes, schema-version
mismatches, and corrupt media fail loud at their owning boundary.
Backend withdrawal removes visibility under the hub lock and destroys the
removed backend after unlocking, including any dependent leases it owns.

One unit descriptor declares its identity, current and compatible older
versions, tables, global slot and requested JSON layout. Business record keys
are UTF-8 strings bounded to 256 bytes; they are never filesystem paths or SQL
identifiers. A global address is distinct from a table address, including an
empty table key or a JSON null value. SQLite stores addresses in separate
table-name and key columns; per-record JSON uses hashes and verifies the
identity in each envelope.

Backends accept immutable bounded compact JSON bytes. A batch contains unique
addresses and either a replacement or deletion for each. SQLite and single
JSON commit the entire batch atomically. Per-record JSON supports single writes
and rollback-capable delete-many, but rejects arbitrary atomic batches before
writing. Consumers requiring batch atomicity check this capability at open.
Read admission accepts only the current version or an explicitly declared older
version. Reads do not rewrite version stamps; an actual mutation stamps the
current version together with its data, leaving untouched payloads unchanged.
The current codec must therefore accept these mixed-generation payloads.

Only a table declared disposable may request backup-and-skip for malformed
records, and only a backend that can preserve its individual source supports
that policy. Globals and authoritative tables reject corruption. Unknown
versions are never treated as disposable corruption. Preservation must finish
durably before a record is skipped.

Deterministic consumer tests use `rsi-storage-testkit` at the backend seam and
exercise the production typed Domain queue. The test backend has no filesystem
or live-service access; it supplies explicit load/write gates and injected
failures. Production composition never selects it implicitly.
