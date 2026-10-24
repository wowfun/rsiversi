---
name: Reuse maintenance reference proofs within a Store generation
---

## Problem

Auditing every surviving canonical record before each single-object CAS collection
makes a page of collections repeatedly scan the whole Store. Sharing that audit
with cold Session validation also delays unrelated Session work.

## Decision

The SQLite Store owns one ephemeral coverage proof tied to its writer commit-attempt
counter and SQLite data version. Its writer lane holds one immediate transaction
through cold audit and proof publication.
Before strict reference admission, a write invalidates it. Once strict admission
is committed, successful typed Store transactions preserve coverage and advance
the proof. Rolled-back transactions preserve it; failed commit attempts,
unaccounted changes and reopen require fresh validation. SQLite total changes
includes rolled-back rows and therefore cannot identify committed state changes.
Strict admission commits in the same transaction as blocker-free purge or CAS
collection admission, with any necessary audit inside that same immediate
transaction. A refused request cannot change later append behavior.
A current strict proof also covers the absence of incomplete reference rows.
The owning [Store contract](../../../../crates/rsi-agent/store-sqlite/README.md)
defines admission, publication and selected-row checks.

Session response deadlines report an unknown outcome without releasing actual
Store ownership. Navigation cleanup follows the Store operation and does not
occupy its maintenance permit. Uncertain admission retains its closing owners and
kernel reservations until an ordered Store binding read confirms absence or
retirement. This internal resolution does not depend on the requesting caller's
current execution grants; returning receipts still requires authorization.
Absence reopens only a fresh attachment generation. Latching permanent
retirement before that evidence could strand a Session without any durable receipt.

## Alternatives considered

A persistent boolean cannot authenticate data across reopen. A proof that trusts
every database write cannot detect writes outside the Store transaction path.
Reauditing every object repeats work already maintained by strict typed writes.
Releasing a mutation permit on timeout allows a second operation to race native
work that may still be running. None of these alternatives preserves both the
ownership contract and proportional maintenance cost.

## Consequences

The first successful audit still reads the whole Store and temporarily queues
writers. Repeated optimistic audits could starve maintenance under continuous
appends, so ordinary readers remain independent while cold audit excludes writes.
Strict admission still
requires exact Domain codecs for new domain records: an opaque record could
otherwise introduce a reference to an already retired object. Domain identities
remain in canonical durable records; missing-codec errors identify the required
name and version. Purge batches retain bounded binding and selected-record checks
so a cached global proof does not replace retirement receipt validation.
Retirement stores the original selection and only execution locations for later
authorization. Complete Headers would retain deleted settings and workspace paths
indefinitely. Releasing the minimal binding on a timer would make idempotent status
and retries dependent on process timing, so it remains with the tombstones.
Per-record digests remain necessary after purge has removed part of a prefix:
the complete prefix chain can no longer authenticate surviving rows then.
