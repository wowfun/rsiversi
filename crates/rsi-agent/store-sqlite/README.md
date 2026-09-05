# rsi-agent-store-sqlite

SQLite and filesystem-CAS ordinary plugin for
`rsi-agent-store-protocol`. Opening the Store acquires one cross-process writer
lease for the entire root before schema validation or recovery reads. Only the
exact current schema is accepted; this pre-release implementation does not
migrate old layouts. Open validates root ownership and the exact schema without
scanning dormant session history. The first header, Fact, turn, checkpoint, or
append access to an existing session validates that session's bounded Header,
durable watermark, Fact-prefix digest shape, and Fact/turn index relationships
inside one read snapshot. A bounded 256-session recency cache avoids repeating
that work; eviction only causes revalidation. This lazy check does not decode
every Fact JSON body, but its watermark count and turn-membership queries scan
the selected session's Fact/turn index ranges and therefore cost O(that
session's durable history) on an uncached first access. Recent listing validates
every uncached returned session in its original snapshot, reuses prior cached
proofs, and refreshes the recency cache.
`SqliteStore::verify` is the
explicit no-create full-store check for SQLite integrity, foreign keys, all
bounded Headers, mechanical watermarks, recomputed canonical Fact-prefix
digests, Fact/turn relationships, and per-root Agent-tree cardinality. The audit streams and validates every
Fact body. Mailbox-index verification compares each canonical control with its
indexed row and a final cardinality check; as an explicit offline whole-store
audit, it currently retains the bounded payload for every accepted message in
one Rust map. It opens the existing writer-lock file and database read-only,
performs no writes, and does not perform WAL recovery. A nonempty WAL makes the
audit fail explicitly because the immutable read-only connection cannot inspect
that committed tail; run it against a cleanly closed Store or a standalone copy
produced with SQLite's backup facilities. This audit covers the complete SQLite
logical state; CAS objects remain validated on each exact read rather than by
`verify`. Kernel recovery owns paged Fact semantics, while each CAS read validates
the exact requested body digest. Indexed boundary reads compare every decoded Fact's
sequence, turn, and kind with the relational row that selected it. Recent
listing returns validated bounded Headers from its original read snapshot
instead of requiring one later reader job per row. Header, Fact, control,
mailbox-message, and mailbox-state reads project SQLite byte length and return
no TEXT body to Rust when that row exceeds its protocol bound. Mailbox payload
and summary reads also pass through the same lazy per-Session validation gate
as other public reads. CAS publication never deletes caller or unrelated files.
It stages publication in a dedicated private directory that is reset after the
writer lease is acquired on open, so a process crash cannot retain partial
staging files indefinitely. A crash after the immutable digest file is
published but before its SQLite metadata commits can retain an unreachable
complete object; ordinary open deliberately does not scan the unbounded CAS
directory to reclaim it.

The configured root must be on a filesystem that honors the host's exclusive
file locks, same-directory atomic rename, and file/directory sync semantics.
Network or shared filesystems that weaken those operations are outside this
backend's durability and single-writer contract; the Store does not infer their
behavior from a path string.

SQLite owns one serialized writer connection and one serialized read-only,
no-create reader connection. Multi-statement reads, including fork selection,
use a deferred transaction so watermarks and rows come from one WAL snapshot.
Descendant control snapshots drive lookups from the bounded recursive result
rather than scanning the complete sessions table. Cursor-paged ready-message,
Agent-child, waiting-activation, and ready-root reads select distinct first-page
and continuation SQL. A continuation keeps the complete cursor tuple as an
index range constraint rather than hiding it behind a nullable `OR` predicate.
CAS file work has a separate single-slot blocking admission. Hashing and
immutable file publication do not hold a SQLite connection mutex; metadata is
checked or inserted only after the file phase completes.

The two connections share one lifetime: clean shutdown closes the reader first
and the writer last, checkpointing the WAL into `sessions.sqlite3`. The main
database is therefore a complete standalone copy after the final Store handle
and operation close. A live backup must still use SQLite's backup facilities or
capture the database and WAL consistently. Open initializes only a missing or
zero-length database; an existing nonempty database without the exact schema is
rejected instead of being republished as an empty Store.

On Unix, owned Store and CAS directories are created and tightened to mode
`0700` before database, writer-lock, or CAS files are opened. Every SQLite
connection also opens the database with `SQLITE_OPEN_NOFOLLOW`, closing the
final-component symlink window after the path precheck.

The exact schema version 11 admits only the current mandatory Agent-preset
Header encoding, indexes Fact rows by turn, advances a Store-owned
canonical Fact-prefix digest with every append, and tracks which accepted
turns do not yet have a terminal Fact. Agent-node root/path lookups have one
schema-owned index, and mailbox rows project the validated message-source class
needed by metadata-only completion-summary reads. Index maintenance is atomic with append;
SQLite also stores one bounded opaque Context checkpoint and its Fact-prefix
digest per session behind an exact durable-tail transaction, rejecting metadata
that differs from the Store-owned header and prefix. It does not interpret
checkpoint bytes, effect state, or choose a terminal outcome. Write-behind
timing, interruption recovery, and turn state belong to the Kernel. See [the
Agent architecture](../docs/architecture.md).

Checkpoint reads reassert the stored header fingerprint against the immutable
session header and reject a checkpoint cursor beyond the current durable tail.
They cannot compare the checkpoint's prefix digest with the session's current
tail digest after later Facts have been appended; Context remains responsible
for binding and validating the opaque checkpoint bytes and their exact prefix.
