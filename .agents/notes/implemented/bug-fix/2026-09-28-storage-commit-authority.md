---
name: Storage authority follows admitted work
---

## Problem

Domain loading preceded registry reservation and admitted writes retained records
without retaining domain authority. Cancellation and last-handle release could
therefore create two authoritative snapshots. JSON workers outlived caller-owned
serialization, and a post-rename error left memory behind the visible file.
Unbounded JSON depth admitted values that the same backend could not decode.

## Decision

Reserve weakly registered authority before load and retain it through admitted
publication. Identity-checked destruction removes idle entries. Backend workers
own their operation slot and health through completion; disposal drains them
before registration release. Unknown commit outcomes permanently fence the
backend and its consumers. A lost domain publication task independently fences
its domain authority before releasing the commit slot. Recovery recreates the composition from durable state.
The Domain layer owns one API error projection, returning unavailable for known
pre-commit I/O failures and retaining unknown outcomes, preserving
the ordinary dispatcher's conservative classification of untyped backend errors.
An explicit Profile regrant after a known failed revocation reopens the same task
tracker, so earlier admitted work remains part of the next revocation drain.
Values have a depth limit of 64 and bounded encoding. JSON recovery syncs the
parent of a validated existing file; each published file was synced before rename.
Consumers use Domain health instead of maintaining parallel uncertainty flags.
SQLite retains transaction ownership while attempting COMMIT. Busy or constraint
rejection is a known failure only while that transaction is still active and an
explicit rollback succeeds; a post-commit WAL error remains unknown even if its
code resembles a rejection. Profile grant preflight precedes gate closure, so
revision exhaustion does not silently revoke an unchanged durable grant.
Attention retention evictions are separate acknowledged record commits; a later
failed acknowledgment cannot restore evicted records in the cache. This keeps
retention bounded without claiming multi-record atomicity from a KV interface.
Storage core owns the shared compact record-object accounting used by Domain,
SQLite and retention
projections. Attention retains measured entry sizes rather than encoding its
complete cache on every acknowledgment; durable admission still checks the bounds.
SQLite transactions persist byte accounting beside record counts and reject
projected growth before mutation. Loads validate both counters against compact
rows. The record object excludes the domain-version envelope, so version digit
width cannot change admission. The exact layout includes this byte counter;
an older layout is rejected and retained for operator disposition. No migration
or repair utility is supplied; the [backend contract](../../../../crates/rsi-storage/sqlite/README.md)
defines accepted durable input.
Activation validates persisted counters against bounded keys and stored BLOB
lengths in one read-only snapshot. Plausible but corrupted counters cannot become
write-admission authority. Owned transactions maintain that invariant afterward;
unmanaged writes to an active database do not supply a coherent merge protocol.
SQLite bounds its durable namespace before scanning any domain's records and
admits new names in the same transaction as their first record. Per-domain limits
alone cannot constrain a caller that repeatedly chooses a fresh name; current
product consumers' fixed names do not constrain the independently usable backend.
Existing-domain writes bypass namespace counting, so a full namespace does not
disable its existing data. Empty domains retain schema identity and consume a slot.
Domain input bounds are checked before publication. Their API rejection retains
that known noncommit receipt; folding it into a generic backend failure would
make the mutation dispatcher report uncertainty for an effect never admitted.

## Alternatives considered

Automatic reconciliation would need to rebuild every consumer projection while
preserving admission and generation identity. Restart recovery gives one clear
boundary. Returning success after a failed durability step hides uncertainty.
Keeping every domain strongly resident avoids races but retains historical names.
Disabling decoder recursion limits would accept inputs without bounding stack use.
Re-syncing the file on reopen adds write access without improving recovery for
this single-writer, sync-before-rename protocol.
Inferring old-schema byte totals on every write would avoid a format change but
rescan unrelated durable values. Automatic migration would add a second durable
publication path outside the selected pre-release contract and is excluded.
Lazy accounting of only the domains requested by the current product would admit
unchecked durable metadata elsewhere and leave activation dependent on consumer
order. A finite provider-owned namespace keeps the complete read-only validation
boundary. Its ceiling is an admission policy, not a measured latency target or a
family-wide limit for media already bounded as complete documents.

## Consequences

Unknown outcomes require restart even if the visible write eventually proves
successful. Existing values deeper than 64 are rejected as corruption. Snapshot
and cached-consumer reads can fail when storage is fenced. No external-writer or
cross-process merge guarantee is introduced. Deterministic cancellation, reopen,
rollback and post-publication fault tests exercise the ownership boundaries.
Existing SQLite files without aggregate byte metadata cannot activate this
backend. No online or offline migration utility is supplied by this change.
Backend uncertainty affects every domain sharing that backend, including
authentication and configuration reads. A domain publication-task failure alone
fences only that domain. Serving last-confirmed configuration for diagnosis would
require a distinct stale-view contract; ordinary reads do not authorize or present
that state as current after an unknown commit.
SQLite files above the namespace ceiling cannot activate; they remain unchanged
for operator disposition. Record deletion does not remove domain schema identity,
so it cannot reclaim a namespace slot. The accepted worst-case startup scan remains
proportional to the bounded domain and record counts; no latency guarantee follows.
