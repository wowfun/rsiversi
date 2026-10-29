---
name: Bound materialization and document cleanup at their owners
---

## Problem

Worst-case Fact reservations serialized small Kernel reads before SQLite's single
foreground reader serialized them again. Checkpoint maintenance disappeared under
otherwise supported memory limits. In the document, awaiting remote closure
blocked local Dock persistence, while late opens and frames could recreate tabs.
GUI serialization also rebuilt unchanged global sections on every frame.

## Decision

Store page contracts admit encoded bytes before Rust payload materialization.
Kernel reads start at 1 MiB and rejoin FIFO admission for one record at its reported
size after a typed first-record capacity refusal. A backwards page can select a
newer, larger record between attempts, so one final maximum-sized attempt bounds
that race without looping indefinitely. Actual workers and queued results retain
their permits. SQLite owns two independent foreground connections and closes an
unhealthy lane instead of recycling uncertain transaction state. Checkpoints
reserve actual bytes; global quota exhaustion is a cache miss. Ordered compact access
projections batch selected authorization reads while caller authority remains held.
Activity uses the required access projection and proof-retaining batches of durable
cuts, reducing reader dispatch without skipping history validation. Access still
decodes bounded Headers in SQLite. Stable operation labels identify measured lane
work; return types and page sizes do not identify caller traffic classes.

Each document connection owns bounded open and cleanup responsibilities. The
resource-open receipt identifies a view incarnation independently of frame arrival.
Dock persistence reserves cleanup before committing and publishes local closure
before remote work. Terminal closure detaches the captured attachment. Failed or
uncertain cleanup remains visible. Exact identities permit retry; unknown opening
outcomes require a matching frame or settled connection teardown. Known allocation
refusals release the reservation. Replaying a rejected opening promise cannot
recover an unknown identity, so the UI must not offer that as a retry. Only settled
connection teardown retires remaining responsibilities. Settings has one local
window with separate sections and preserves exact JSON drafts. A fresh ticket can
resume a retained draft only at the same actual Settings version. Conflicting
versions require explicit discard rather than silently rebasing user edits.

Presentation owners advance revisions synchronously under the state lock.
Successful frame encoding commits section caches and action authority together.
Composite fields from a shared owner retain one guard through capture.
Context accounting distinguishes ordinary Turn, usage, and summary-wide changes.
Current contracts live in the owning Store, Kernel, Context, GUI and Web READMEs.

Paired verification also exposed two publication-boundary defects: the Web
builder generated a different bundle identity, and daemon publication copied a
managed executable away from its required lease. The fixture producer supplies
the captured family as Web bundle identity. Managed Host launch retains the
original immutable publication and a lease through readiness; the child acquires
its own lease. Unmanaged Cargo outputs retain private runtime publication.

## Alternatives considered

Increasing only the byte cap leaves SQLite serialized; increasing only SQLite
connections leaves Kernel reads serialized. Unconditional checkpoint reservations
waste shared capacity, while treating local Context pressure as a cache miss hides
its intended bound. A large general connection pool adds no demonstrated value.

Coordinate-based resource matching cannot distinguish duplicate documents or
reversed open replies. Dropping failed cleanup loses ownership; awaiting it before
local persistence makes network health a prerequisite for dismissal. A background
change notification is unsuitable as a content-cache revision because publication
can lag the state write. Global cache invalidation is retained only where an owner
cannot cheaply distinguish its related fields.

Removing the compiled managed-lease requirement would hide an invalid artifact
layout and weaken collection safety. Copying a marker into a runtime cache would
also misrepresent its ownership. Managed generations remain under their existing
application publication collector, not the private Host runtime collector.

## Consequences

These pre-release Store and document wire changes require rebuilding every adapter;
there is no compatibility shim. Two readers increase SQLite connection/cache cost.
Large-record fallback can wait behind earlier readers, by design. An unknown open
can consume bounded capacity until connection teardown; the UI reports it rather
than replaying a possibly successful operation. Cold maintenance reference proof
still holds the writer through proof publication: removing that authority would
invalidate its progress guarantee. Performance evidence must distinguish these
structural bounds from measured latency.
