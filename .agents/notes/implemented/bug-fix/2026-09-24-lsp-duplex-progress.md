---
name: Bounded LSP progress during pending writes and idle periods
---

## Problem

Writing a complete document before consuming server stdout creates a circular
wait with Process's bounded output queue. Awaiting a server-request reply inline
can recreate the same cycle. Legitimate traffic below the query budget stalls.

## Decision

One connection owner preserves read progress without relinquishing an accepted
write prefix. Compact reply descriptors bound memory while the write lane is busy.
Idle traffic needs an aggregate budget even when every individual reply drains;
a quiet pooled process remains reusable without a wall-clock expiry. Failed
settlement remains retained and closes admission without replacing an existing
query's primary error. Rejoining a Process provider that caches a terminal failure
cannot repair an operating-system failure; the owner must continue reporting it.
The [language contract](../../../../crates/rsi-lsp/core/README.md) owns admission,
budgets and retirement. A response deadline retires the whole connection because
silently dropping an admitted JSON-RPC response would strand the server, while
interrupting a partially written frame would corrupt the stream.

## Alternatives considered

Larger pipes only move the deadlock threshold. A task per notification or an
unbounded RPC map would discard existing retention guarantees. Recreating a write
future after every incoming message risks replaying an accepted prefix. Document
caching needs independent freshness and traffic evidence and is deferred.

## Consequences

An idle connection owns a lightweight task and can fail before the next query.
Only a new query reconnects after joining that failed owner. Bounded real-pipe
tests cover both document and server-reply pressure; portable pump tests cover
prefix ownership and budgets. These do not prove every language server's behavior.

Concurrent fake-server fixtures also share LocalFiles' process-wide four-job budget.
Diagnostics reproduced `FilesError::Capacity` at source open before the protocol
operation. Bound fixture concurrency to that existing limit rather than raising
product capacity or retrying an admitted query. This explains the observed
source-open `Unavailable`; earlier failures without diagnostics remain unclassified.
