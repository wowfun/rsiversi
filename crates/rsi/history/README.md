# rsi-history

ProductHistorySearch owns a rebuildable SQLite FTS5 cache in an exclusively leased
private directory outside the Agent Store root. On Unix, only the trusted first
absolute path component may be a platform root alias; every suffix component is
acquired without following links. It consumes the public Store,
external conversation, Workspace and References contracts. Kernel, Meta and the
Agent Store do not own search semantics or the FTS database. The
[wire contract](../history-api/README.md) owns user-visible bounds and scope.

Two retained workers admit finite operations without an unbounded queue. Dropped
waiters cancel further steps, while dispatched Store, journal and blocking SQLite
work keeps its permit until completion; retirement drains before releasing the
lease. Each native batch admits at most 256 Facts and 16 MiB before body decoding.
Derived index documents additionally stop before an original when reaching 8192
fields or 64 MiB of encoded metadata plus text. One bounded original is staged
before this decision; it is retried by the next batch, never silently omitted.
External pages preserve their own 64-record/256-KiB bound and read at most 16 MiB
through bounded windows. Explicit oversized-record and text omissions advance
coverage without pretending the original was indexed. Only direct human text,
visible assistant text and explicit Tool text are exported; reasoning, provider
requests, permissions and raw Tool JSON are excluded.

The index has a 1 GiB page ceiling and bounded SQLite VM work. It uses DELETE
journaling; no second writer or database is permitted in its dedicated directory.
Source identity participates in the FTS index, so scoped searches and resets do
not scan unrelated documents. A reset first invalidates coverage and cursors,
then commits deletion in batches of at most 128 documents, each with its own VM
budget and cancellation check. Interrupted cleanup remains hidden and resumes
before the next indexing batch; it cannot publish old documents as new coverage.
A malformed database or obsolete schema is rebuilt under the lease, with a new generation,
without touching source truth. Cache entries cannot authorize original reads or
supply frozen text. A validated original is always reread at its exact identity.

The `history_search` Tool derives workspace and target from AgentCallerAuthority,
uses the same owner operations, and emits the `rsi.history` version-1 typed output.
Its original-text window is capped at 32 KiB so worst-case JSON escaping remains
inside the Tools output bound; subsequent reads retain exact source coordinates.

Routine cache opening validates the exact schema and bounded generation metadata;
it never scans document contents or runs FTS integrity checks. Only an obsolete
schema or confirmed malformed database triggers reconstruction. Resource limits,
SQLite interruption and I/O failures leave the existing cache in place. SQLite
disk/page exhaustion is reported as Capacity.

Search admits result coordinates by their stored encoded metadata lengths before
fetching that bounded set in one ordered query. JSON escaping is charged in these
lengths; an oversized aggregate page retains a continuation after its last hit.
