# rsi-storage-sqlite

This ordinary backend plugin stores routed non-session domains in one explicit
SQLite database. It creates a strict versioned schema, enables foreign keys,
checks each domain schema version, and commits each record mutation in one
transaction. One async operation slot is acquired before a blocking SQLite task
is created, bounding work queued against the single connection.
Newly created path components and the database are private on Unix; existing
caller-supplied parent directories retain their permissions. The database and
SQLite sidecars must be real regular files rather than symbolic links.

The connection waits for a bounded busy interval when another SQLite writer
temporarily owns the database. Loads reject an oversized durable BLOB from its
stored length before materializing it as an owned value. The write transaction
rejects a new key when its domain already has 65,536 records, so the raw backend
cannot durably create a domain that its own load boundary must reject.

It is not the Agent session store and carries no session recovery semantics.
