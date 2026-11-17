# rsi-storage-domain

Consumers declare typed tables and an optional global slot with an exact backend
route and schema version. Serde codecs decode at the durable read boundary;
trusted typed writes encode once without re-decoding or an intermediate JSON
tree. Codecs must serialize values their own decoder accepts and must not expose
interior mutation of persistent fields. Immutable records
and snapshots share allocations; snapshots stay stable across later writes.
Reads perform no I/O and check retained generation health. Global set returns
the shared committed value so a consumer can publish its accompanying cache
without copying the business document. Retaining a table snapshot across writes
requires copying that table's key map on its next mutation; values remain shared.
Release old snapshots when their stable view is no longer needed.
Domain record/byte admission precedes
business deserialization; rejected oversized records never invoke their codec.

Each domain has one worker and admits at most two writes at once: one executing
and one queued. Further callers wait in FIFO order before single-value encoding
or delete-many preparation. A caller owns any batch it assembled before admission.
Cancellation while waiting submits nothing; close wakes those waiters with `Closed`.
Accepted writes retain domain
ownership through durability, in-memory publication and event emission even if
the requester is cancelled. Update closures are synchronous pure transforms of
the value current at their queue slot; missing records reject without a write.
Delete-many deduplicates keys, ignores missing records and uses one queue slot.
Atomic batches are available only when the backend supplies that capability.
The domain applies exact record and encoded-byte bounds before durable work.
A batch has at most twice the declared record capacity, allowing a complete
atomic replacement. `Domain::batch` rejects oversized caller-owned batches before
waiting for a queue slot. Delete-many acquires its preparation slot first and
applies the same bound to distinct keys while collecting its finite, in-process
iterator; callers own the iterator's execution cost.
Validation reads an immutable snapshot outside the publication lock. The sole
writer retains that version until commit, and concurrent snapshot readers do
not wait for batch validation.

Close immediately rejects new writes, drains accepted work and releases the
backend unit, reserved name and event sender before its future completes. The name can then
be reopened while closed handles remain alive. Close is idempotent and its drain survives waiter
cancellation. Facility disposal closes every remaining domain before withdrawing.
Already accepted writes may publish while close drains; reads reject after the
drain completes. Backend withdrawal and uncertain-outcome fencing remain governed
by the [core contract](../core/README.md).

Concurrent opens reserve authority before loading and share initialization.
Failed or cancelled initialization can be retried. A different specification or
codec type for an occupied name rejects. The registry holds weak authorities;
accepted jobs retain authority, without retaining historical names forever.
A loaded backend generation never changes in place.

Subscriptions belong to an open authority, not a cross-process bus. The bounded
broadcast stream contains only revision, operation and address, never values.
Publication and enqueueing events share the snapshot lock; event delivery never
calls subscribers under that lock or waits for a subscriber. Revisions increase
in commit order. Snapshot-and-subscribe is atomic. Lagging receivers must
resnapshot and subscribe again; old events must not be replayed onto the new
snapshot. Missing-key deletes emit no event.

The [family contract](../README.md) defines layout, compatibility and disposable
record recovery. Compatible versions default to none. Reads do not restamp;
actual writes stamp the current version without rewriting untouched payloads.
Business codecs must accept the declared older shapes even in a unit whose
version has already advanced. Global absence is distinct from a stored null.

The [core accounting contract](../core/README.md) defines exact projections.
Backend failures known to precede commit leave the published snapshot unchanged.
Unknown outcomes fence the entire backend generation; a lost domain worker also
fences its own authority before any further publication or read. Recovery opens
fresh generations from durable state. The shared `storage_error` API projection
preserves `OutcomeUnknown`, maps unavailable and pre-commit I/O failures to
`Unavailable`, input rejection to `Invalid`, and corruption/version errors to
`Backend`. A closed Domain or facility reports the typed `Closed` error, projected
as `ShuttingDown`; retirement is not an unavailable backend route.
