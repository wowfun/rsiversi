# rsi-storage-sqlite

The backend owns `SQLITE_STORAGE_FORMAT`. Its retained writer corpus records the
actual SQLite schema and record bytes. Same-format edits must preserve both;
unsupported corpora remain refusal cases and are never regenerated. Capture is
explicit, writes only to a new directory, and uses the production plugin.

Factory preparation preserves malformed configuration as `MetaError::InvalidInput`
and reports unavailable native capability as `MetaError::Activation`, the Meta
category for plugin preparation and activation failure.

This ordinary backend plugin stores routed non-session domains in one explicit
SQLite database. It creates a strict versioned schema, enables foreign keys,
uses WAL with explicit `synchronous=FULL`,
opens owned units with declared tables and a distinct global slot, and commits
each batch in one transaction. Table name and business key occupy separate
columns; empty columns name the global, including a stored JSON null. Prepared
statements are reused. Unit names remain reserved through actual worker
completion, including cancelled openers and writers. One async operation slot is acquired before a blocking SQLite task
is created, bounding dispatched work against the single connection. Backend
operation concurrency is one, including reads; WAL does not imply a read pool.
Newly created path components and the database are private on Unix; existing
caller-supplied parent directories retain their permissions. Unix activation
requires the database and SQLite sidecars to be real regular files rather than
symbolic links. Windows requires fixed local NTFS and exact no-reparse acquisition.
New database files are private at creation; existing database/sidecar objects are
privatized only after a retained native handle proves a regular file owned by
TokenUser. WAL and shared-memory sidecars are acquired or created with explicit
TokenUser ownership before SQLite opens them, including with an elevated token
whose default object owner is Administrators. Existing foreign-owned sidecars
are refused. Native failures
prevent activation. The final database directory must already have the native
private descriptor (newly created directories do); a NamespaceLease retains it
with the SQLite connection and every blocking worker. Existing higher ancestors
keep their permissions. Other platforms reject before opening SQLite.
Configuration identifiers and absolute-path syntax are checked without I/O
before platform admission: malformed configuration returns `InvalidInput`,
while valid configuration on an unsupported platform returns `Unsupported`.
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
helper and updates count, byte metadata and the current unit version once per
batch. Per-row count triggers are unnecessary because the transaction already
computed the final geometry. Final bounds are checked before row mutations, so
insert-before-delete ordering cannot reject a valid replacement batch. Mutations validate the stored domain
version, accounting and previous record length together before projection.
Deleting a missing domain or key is a no-op; an existing domain must still match
the requested current or explicitly compatible older version. Missing-key
deletes do not advance the version; actual writes stamp current without
rewriting untouched payloads. Loads check the byte and count metadata
against the actual compact values before publication. Raw units accept validated `EncodedValue` buffers and bind their exact bytes.
They neither decode business records nor build or serialize JSON value trees.
Loaded values admit bounded compact framing, then the Domain decodes its business
codec once. Exact accounting uses stored lexeme lengths, without requiring a
second serialization of the decoded business value. Opening verifies domain accounting
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
`table_name`, `record_count` and `record_bytes`; the owning format is version 2.
The exact owned SQL shape is the runtime format discriminator; the retained
writer corpus binds it to that version. Older layouts are rejected without
payload mutation or automatic migration.

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
