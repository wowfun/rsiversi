# rsi-jobs-local

This ordinary plugin implements process-local Jobs on the caller-owned Tokio
runtime. Admission is capped at 10 active jobs per scope and 256 globally by
default. Retention is capped at 256 tombstones per scope and 1,024 globally.
`maximum_scopes` independently caps current scope lookup mappings at 1,024 by
default, within 1..=65,536. It does not count caller-retained historical authority
handles. Reusing a current active scope succeeds even at capacity and examines
only that identity. New mappings below capacity do not scan unrelated entries;
dead or revoked mappings may remain until capacity pressure. At capacity, one cleanup
scan precedes rejection with `Capacity`; this saturated path remains linear in
the bounded backing allocation. Exact finalization removes its own mapping.
Unreported terminal jobs are deliberately not eviction candidates; capacity is
therefore backpressure on consumers that fail to report observable background
work, not silent data loss.

Producer retirement first withdraws its exact generation, then cancels and
waits for that generation's work under the configured shutdown timeout. Scope
finalization revokes its exact authority generation before taking a snapshot.
If an already-admitted terminal read reports an item from that snapshot first,
the reaper skips the now-reported or compacted record instead of reporting it a
second time or failing finalization.
Work started under a reservation remains charged to its scope and producer
until its control settles even when a publication race prevents an identifier
from being returned. The provider owns that unpublished control, cancels it,
and joins it before the corresponding finalizer or producer retirement can
report quiescence. A caller timeout cannot orphan either published or
unpublished work. Provider retirement applies the same rule to every remaining
generation.

Each published record owns its settlement notification. Waiting for one job
does not subscribe to provider-global change traffic; aggregate scope,
producer, and provider finalizers retain their separate bounded lifecycle
notification.

Once a terminal job is reported, the provider releases its producer control and
captured output and keeps a small immutable tombstone. Oldest reported
tombstones are compacted at the configured retention bounds, except while an
already-admitted read holds that exact record. Such a read completes from one
stable record identity even when concurrent settlement and admission would
otherwise make the tombstone evictable. Its active summary is linearized with
the active-status observation under one registry lock; observing terminal
instead causes both stream tails to be read again before the same record is
reported. Per-scope
retention cannot exceed the 256-record list contract; global retention may span
many scopes.
Admission uses maintained per-scope retained counts and sequence-ordered
eviction indexes. Only terminal, reported records with no admitted readers
enter those indexes. Publication, settlement, reporting, and the final read
release update them under the registry lock; revoking a scope leaves its
retained records charged until compaction removes them. Admission therefore
examines only eviction candidates, independently of unrelated retained work.
Preview takes a temporary control snapshot, invokes producers outside the
registry lock, then revalidates the scope and exact record/control before
returning output. It does not hold a read lease or prevent reporting/eviction;
output discarded during the sample remains unavailable.
Producer start, wait, read, peek, and cancel callbacks are panic-contained. No work
or output is recovered after process exit. Producer wait failures are projected
to a bounded failed terminal after NUL removal, so containment never creates a
value that violates the Jobs protocol it is meant to protect.

Scope retention owns ordered membership as well as eviction eligibility. Scope
list and finalization inspect only that generation's members; global withdrawal
still visits all relevant records. Producer entries are moved out of the registry
before cooperative cancellation and opaque destruction run outside its mutex.
