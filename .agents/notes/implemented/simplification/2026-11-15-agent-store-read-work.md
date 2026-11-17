---
name: Bound historical discovery and reuse authenticated read state
---

## Problem

Historical domain discovery enumerates lifetime names; mutation admission reads
snapshots it does not consume. CAS publication hashes the same immutable candidate
twice, once on an async worker. Fork reconstruction repeatedly authenticates the
same ancestry despite an existing Store interval-capture operation.

## Decision

SQLite derives bounded membership cuts from canonical domain controls and
verifies their contents and total count during canonical replay. Mutation
admission reads atomic heads and watermarks without materializing unused
snapshots. A private CAS candidate binds immutable bytes to one digest prepared
on the bounded publication lane; durable bytes remain independently verified.
An operation-owned fork reader reuses Store's existing authenticated interval
capture and validation leases, with current claim and availability checks for
every captured ancestor on each page. Executor reconstruction stops at the known
terminal cut.

## Alternatives considered

Adding a horizon predicate to lifetime name enumeration still scans retired
names. Caching full snapshots duplicates authoritative payloads. A global ancestry
cache adds invalidation state; operation-local Store validation leases already
express the required lifetime. Increasing worker counts does not remove duplicate
hashing or metadata-only consumers' payload decoding. Weak reader ownership
would permit Store shutdown during an admitted reconstruction. Omitting the
post-page checkpoint check would return a final page even if its maintenance
Session became unavailable during the read; a later page need not exist.
Reserving the whole maximum Header chain against transient Store-read admission
would serialize even short captures without bounding reader retention after
capture. Per-Header read admission and the existing inherited-depth bound remain
separate limits.
Proof retention refuses capacity instead of queuing: concurrent nested captures
can each hold partial ancestry and otherwise wait forever for one another's
slots. Existing same-Session pins remain shareable at capacity. The existing
`ValidationBusy` path lets the caller unwind its partial capture and retry;
no pin notification queue or new error category is needed.

## Consequences

Schema 51 refuses older files without rewriting them. Membership metadata stores
one bounded baseline name set and subsequent membership changes; selected historical lookup work is
independent of lifetime name churn, while mandatory cold integrity replay still
depends on history. CAS hashing and publication remain separate blocking jobs
because digest-based ownership admission falls between them. Inherited proof
leases last for one reconstruction rather than one page; they prevent proof-cache
eviction but do not prevent Session retirement. Readers retain Kernel and Store
ownership, including SQLite's writer lock, until their last reference is dropped.
No global cache, additional
ancestry representation or invalidation protocol is introduced.
