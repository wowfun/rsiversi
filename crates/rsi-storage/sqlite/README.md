# rsi-storage-sqlite

This ordinary backend plugin stores routed non-session domains in one explicit
SQLite database. It creates a strict versioned schema, enables foreign keys,
uses WAL with explicit `synchronous=FULL`,
checks each domain schema version, and commits each record mutation in one
transaction. One async operation slot is acquired before a blocking SQLite task
is created, bounding dispatched work against the single connection. Backend
operation concurrency is one, including reads; WAL does not imply a read pool.
Newly created path components and the database are private on Unix; existing
caller-supplied parent directories retain their permissions. Unix activation
requires the database and SQLite sidecars to be real regular files rather than
symbolic links; the provider does not enforce that native file check off Unix.
Opening creates and canonicalizes the parent once, then uses that fixed parent
for the database and every sidecar; ancestor aliases are permitted.

Mutations acquire an IMMEDIATE transaction before reading accounting, so another
connection cannot invalidate a deferred read-to-write upgrade. The connection
waits for a bounded busy interval when another SQLite writer temporarily owns
the database. Each load reads its header and records in one read transaction.
Loads reject an oversized durable BLOB from its
stored length before materializing it as an owned value. The write transaction
rejects a new key when its domain already has 65,536 records, so the raw backend
cannot durably create a domain that its own load boundary must reject.
The same transaction projects aggregate record-object bytes through the core
helper and updates durable byte metadata. Mutations validate the stored domain
version, accounting and previous record length together before projection.
Deleting a missing domain or key is a no-op; an existing domain must still match
the requested schema version. Loads check the byte and count metadata
against the actual compact values before publication. Raw `KvBackend::put` accepts
a typed JSON value and uses the core's bounded compact encoder before dispatch;
it does not accept caller-supplied BLOB bytes. Opening verifies domain accounting
against bounded stored keys and BLOB lengths before mutations can use it, without
materializing BLOB bodies. Activation also rejects zero or out-of-range domain
versions and invalid counters as `Corrupt`, retaining the database unchanged.
Loads additionally validate JSON and compact encoding.
One database retains at most 1,024 distinct domains, including empty domains.
Creating a new domain checks this ceiling after its row insertion inside the
mutation transaction and
returns `InvalidInput` without committing when full. Existing-domain updates,
insertions and deletions remain available at the ceiling; deleting its last record
preserves the domain's schema version and does not release a domain slot.
Activation first checks the domain count using at most 1,025 indexed entries and
rejects excess durable domains as `Corrupt` without rewriting the database or
scanning their records. Subsequent accounting scans at most 1,024 domains and
65,537 records per domain, including the overflow sentinel. Startup work grows
with the accepted persisted domain and record counts; these are cardinality bounds,
not a startup latency guarantee.
Unmanaged edits while a backend generation is active have no coherence guarantee.
The exact schema includes
`record_bytes`; older layouts are rejected without mutation or automatic migration.

It is not the Agent session store and carries no session recovery semantics.

A transaction-body error is returned as an ordinary failure only after successful
rollback (including SQLite automatic rollback). A COMMIT rejected with a busy or
constraint error while the transaction remains active is also an ordinary I/O
failure after confirmed rollback. Other commit errors and failed rollback return
`OutcomeUnknown` and fence the connection until a fresh generation opens. Error
codes alone do not establish rejection: a WAL callback may fail after commit.
Blocking workers own admission until transaction completion, including when the
requesting future is cancelled.

On Unix, directory creation synchronizes every ancestor entry before publishing
backend state, including an existing ancestor left by an interrupted creation.
Directory-sync failure prevents a successful first publication.
