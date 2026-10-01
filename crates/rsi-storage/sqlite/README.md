# rsi-storage-sqlite

This ordinary backend plugin stores routed non-session domains in one explicit
SQLite database. It creates a strict versioned schema, enables foreign keys,
checks each domain schema version, and commits each record mutation in one
transaction. One async operation slot is acquired before a blocking SQLite task
is created, bounding dispatched work against the single connection. Backend
operation concurrency is one, including reads; WAL does not imply a read pool.
Newly created path components and the database are private on Unix; existing
caller-supplied parent directories retain their permissions. The database and
SQLite sidecars must be real regular files rather than symbolic links.
Opening creates and canonicalizes the parent once, then uses that fixed parent
for the database and every sidecar; ancestor aliases are permitted.

The connection waits for a bounded busy interval when another SQLite writer
temporarily owns the database. Loads reject an oversized durable BLOB from its
stored length before materializing it as an owned value. The write transaction
rejects a new key when its domain already has 65,536 records, so the raw backend
cannot durably create a domain that its own load boundary must reject.

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
