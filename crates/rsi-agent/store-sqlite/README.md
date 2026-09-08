# rsi-agent-store-sqlite

SQLite and filesystem-CAS ordinary plugin for
`rsi-agent-store-protocol`. Opening the Store acquires one cross-process writer
lease for the entire root before schema validation or recovery reads. Only the
exact current schema is accepted; this pre-release implementation does not
migrate old layouts. Open validates root ownership and the exact schema without
scanning dormant session history. Header and recent-session reads validate only bounded immutable metadata.
Explicit `validate_session`, Fact, control, turn, checkpoint, and append access
validate the selected session's mechanical durable invariants in one snapshot.
A bounded 256-session recency cache and one serialized validation lane avoid
repeating that work; metadata reads neither fill nor touch the cache. The
cache is an optional hint: a poisoned cache disables proof reuse and marking,
so it cannot turn a successful durable commit into an error. Cold validation
still runs before any indexed execution/history read.
Every canonical terminal Fact must have the matching non-NULL terminal index
sequence and prefix digest. Missing terminal metadata is corruption in both
online indexed reads and offline verification; it never describes an open Turn.
The lazy
check does not decode every Fact JSON body, but its watermark count and
turn-membership queries cost O(that session's history) on an uncached access.
It streams and decodes each canonical Agent control once, feeding separate
mailbox, ready, and active-activation projections. Completed pending payloads
are released during that pass. All projections borrow the same immutable Header
already decoded in that validation transaction; control history length does not
multiply Header reads. Activation guards require this proof even when
the guarded Session is outside the write set.
Subtree reads and transactional quiescence guards require that same proof for
every previously unvalidated member. A foreground subtree snapshot with a
missing proof is discarded and collected again on the validation connection;
validation and the returned activity flags share that new snapshot. A guarded
writer transaction discovers missing proofs before applying writes, releases
the writer, validates those members, and retries the original compare-and-append
request. Its operation-local proof set is bounded by the tree limit, and each
validation retry adds a distinct immutable member. The final activity check
runs inside the write transaction, including children created by that commit.
Successful typed writes preserve the proof; external writes
while this Store holds its exclusive lease are outside that ownership contract.
Proofs obtained after applying a transaction enter the cache only after commit,
so rollback cannot publish validation of discarded state.
`SqliteStore::verify` is the
explicit no-create full-store check for SQLite integrity, foreign keys, all
bounded Headers, mechanical watermarks, recomputed canonical Fact-prefix
digests, Fact/turn relationships, and per-root Agent-tree cardinality. The audit streams and validates every
Fact body. Mailbox-index verification compares each canonical control with its
indexed row and a final cardinality check. Replay retains only bounded pending
message payloads; completed entries are compared and released as they close. It opens the existing writer-lock file and database read-only,
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
no TEXT body to Rust when that row exceeds its protocol bound. Fact and
control-page aggregate admission also uses the stored byte length before
copying or decoding the next body. Unexpected SQLite column types, including
NULL in a required column, are corruption rather than retryable I/O failures.
Mailbox payload and summary reads also pass through the same lazy per-Session validation gate
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

SQLite owns one serialized writer, one foreground reader, and one validation
reader. Both readers are read-only and no-create. Each lane admits at most one
blocking job; queued async callers do not occupy blocking threads. Validation
rechecks the proof cache after admission and publishes a successful proof from
the admitted worker even if its waiter has been cancelled. Long cold scans
therefore do not hold the foreground reader or writer.
Multi-statement reads, including fork selection,
use a deferred transaction so watermarks and rows come from one WAL snapshot.
Descendant control snapshots drive lookups from the bounded recursive result
rather than scanning the complete sessions table. Cursor-paged ready-message,
Agent-child, waiting-activation, and ready-root reads select distinct first-page
and continuation SQL. A continuation keeps the complete cursor tuple as an
index range constraint rather than hiding it behind a nullable `OR` predicate.
CAS file work has a separate single-slot blocking admission. Hashing and
immutable file publication do not hold a SQLite connection mutex; metadata is
checked or inserted only after the file phase completes.

Every admitted blocking database or CAS job retains the complete Store owner,
including its writer lease, even after its async waiter is cancelled. The three
connections share that lifetime: clean shutdown closes both readers first
and the writer last, checkpointing the WAL into `sessions.sqlite3`, then explicitly
unlocks and closes the persistent writer-lock file. The lock path is never removed.
The main
database is therefore a complete standalone copy after the final Store handle
and operation close. A live backup must still use SQLite's backup facilities or
capture the database and WAL consistently. Open initializes only a missing or
zero-length database; an existing nonempty database without the exact schema is
rejected instead of being republished as an empty Store.

Schema validation compares SQLite's stored DDL exactly with the declarations
used at creation. It never normalizes literals, whitespace, or punctuation.

On Unix, owned Store and CAS directories are created and tightened to mode
`0700` before database, writer-lock, or CAS files are opened. Every SQLite
connection also opens the database with `SQLITE_OPEN_NOFOLLOW`, closing the
final-component symlink window after the path precheck.

The exact schema version 12 admits only the current mandatory Agent-preset
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
