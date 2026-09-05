---
name: Session-local Store validation and read snapshots
comment: Separate fast availability from explicit whole-database audit
---

## Problem

Opening the SQLite Agent Store ran physical and logical scans over every Fact
ever retained even though startup recovery selects only open sessions. One
mutex-protected connection also serialized independent reads with writes.
Durable history grows per model event, so availability and read concurrency
scaled with unrelated dormant history.

## Decision

Open retains the exclusive writer lease, owned-root checks, and exact schema
validation but does not scan session contents. First access to an existing
session validates its bounded Header, watermark, digest shape, and Fact/turn
relationships in one deferred read transaction. One async single-flight gate
and a 256-entry recency cache reuse that proof; new sessions enter the cache
only after their creation transaction commits.

The adapter owns one writer and one read-only, no-create, query-only connection
in one shared connection pair. After the last Store clone and in-flight
operation release that pair, clean shutdown closes the reader before the
writer so SQLite checkpoints the WAL into the main database. A nonempty
pre-existing database is never treated as an uninitialized Store.
Multi-statement reads use one deferred snapshot. `SqliteStore::verify` acquires
the writer lease, opens only an existing Store, and performs exact schema,
physical integrity, foreign-key, and complete logical checks without creating
database or CAS paths. It decodes every bounded Header and Fact and recomputes
each canonical Fact-prefix digest in addition to applying the same watermark
and turn-index invariants as lazy validation. The full audit also groups the
Agent lineage index by root and reasserts the 256-Session tree bound; insert-time
admission is not treated as proof after durable bytes may have been corrupted.
Because its immutable read-only
connection cannot inspect a WAL, verification refuses any nonempty WAL instead
of auditing a stale main database or mutating the Store by checkpointing it.
Mailbox payload and metadata-only summary reads use the same first-access
Session validation gate. SQLite projects the encoded length before returning a
Header, Fact, control, mailbox message, or mailbox lifecycle body, and omits an
oversized value rather than materializing it in Rust.
The Store interface also provides one exact indexed
turn-boundary read while Kernel retains outcome interpretation.
Atomic appends without a new Header require an existing Session and return
`NotFound` before comparing absent Fact/control tails. The in-memory Store
fixture carries the same ordering so it cannot hide backend-specific branches.

## Alternatives considered

A clean-close bit that skips validation was rejected because nominal shutdown
cannot prove an uncorrupted Store. A WAL-presence check is used only to reject an
incomplete offline-audit input; it never substitutes for the full validation.
A configurable pool was rejected because one writer plus one snapshot reader
provides the required concurrency without tuning policy. Full validation
at open was rejected because dormant history would remain an availability gate.

Activation lineage is derived from immutable Headers at control admission and
offline audit. A matching control digest and activation index cannot substitute
for that independent relationship check. First access also checks the shape of
the control-prefix digest and the active index's parent against the Header.

## Consequences

Dormant corruption no longer blocks open or unrelated valid sessions; first
access to the bad session and explicit offline verify both fail. Cache eviction
causes revalidation and concurrent first access runs one validation. Readers
observe either a complete pre-commit or post-commit WAL snapshot. Outcome
lookup uses accepted and terminal primary-key rows rather than scanning middle
Facts. After clean shutdown, `sessions.sqlite3` is a complete standalone
database; while the Store is open, backup tooling must snapshot the live SQLite
database and its WAL consistently rather than copying one file.

Physical corruption in an untouched page is detected only when SQLite reads
that page or an operator runs `rsi agent-store verify`. This is the intentional
availability tradeoff and open is not represented as a full durability audit.
The same startup boundary excludes a complete CAS-directory scan. Staging files
are bounded and removed on reopen, but a crash after a digest file is published
and before its metadata insert commits can leave an unreachable complete file.
Eliminating that residue requires a recoverable filesystem/SQLite publication
protocol or an explicit bounded maintenance operation, not eager whole-CAS
validation in ordinary open.
