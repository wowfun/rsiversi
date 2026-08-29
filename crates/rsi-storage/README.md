# rsi-storage

`rsi-storage` is the non-session domain-storage family. It is deliberately not
the Agent session store and is not a generic persistence escape hatch for
Settings, Credentials, or Media.

The [`rsi-storage`](core/README.md) plugin owns a process-local backend hub.
Backend plugins register exact names with generation-owned leases. The
[`rsi-storage-domain`](domain/README.md) plugin opens bounded JSON record
domains with explicit record and aggregate-byte ceilings, serializes writes per
domain, makes a backend write durable first, and only then publishes the new
in-memory snapshot. [`rsi-storage-json`](json/README.md)
and [`rsi-storage-sqlite`](sqlite/README.md) are explicit local backend plugins.
Every backend also enforces the family-wide 65,536-record ceiling at raw `put`:
updating an existing key remains valid at the ceiling, while inserting another
key fails before commit.

Complete backend loads reject the absolute 256 MiB encoded domain ceiling
before decoding the body that would cross it. SQLite activates only its exact
STRICT schema, constraints, indexes, foreign key, and schema version; an
existing lookalike layout is corruption, not an implicit migration source.

There is no implicit backend, path, format migration, cross-process merge, or
fallback routing. Duplicate backend names, missing routes, schema-version
mismatches, and corrupt media fail loud at their owning boundary.
