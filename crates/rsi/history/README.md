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
Malformed source bodies instead leave that source unavailable without advancing
its confirmed horizon; repairing durable truth belongs to the source owner.

The index has a 1 GiB page ceiling and bounded SQLite VM work. It uses DELETE
journaling; no second writer or database is permitted in its dedicated directory.
Source identity participates in the FTS index, so scoped searches and resets do
not scan unrelated documents. A reset first invalidates coverage and cursors,
then commits deletion in batches of at most 128 documents, each with its own VM
budget and cancellation check. Interrupted cleanup remains hidden and resumes
before the next indexing batch; it cannot publish old documents as new coverage.
A batch with no documents and unchanged coverage performs no cache publication
and preserves query cursors. Deferred reset cleanup still runs before this check.
A malformed database or obsolete schema is rebuilt under the lease, with a new generation,
without touching source truth. Cache entries cannot authorize original reads or
supply frozen text. A validated original is always reread at its exact identity.

Every operation carries either the actual API origin or the current Agent caller.
Discovery continuations bind that principal; authenticated devices use their
canonical DeviceId, independent of diagnostic formatting.
Before source bodies or cache contents are accessed, the registered workspace's
execution location must admit that origin. Agent requests additionally match the
caller's exact coordinates and retain its current execution lease, never a Local
Service grant. The accepted finite worker retains the location permit through
actual source/cache settlement. Range scans share that retained permit among
sources at the same execution location, with fresh admission checks before each
source read and before publishing the result. Catalog size therefore does not
consume one simultaneous execution slot per conversation. Offline metadata authority suffices for API
history reads; searching never connects an SSH target. A stale cache or hit grants
no access after Use withdrawal. Freeze independently admits the source and actual
receiving Header through caller-scoped Session ingress. A typed in-process capture
context retains both permissions and binds their identities; capture follows
[References' protection rules](../../rsi-agent/references/README.md). References owns integrity and CAS; this product owns
grants.
Session authentication refusals retain Unauthorized; unavailable Session identities
and backend reads surface Unavailable through the History API.

The `history_search` Tool derives workspace and target from AgentCallerAuthority,
exposes discover/query/progress plus read/freeze/rebuild, and emits the
`rsi.history` version-2 typed output. Discover alone advances finite indexing;
its continuation is reused with discover. Tool arguments put operation,
conversation, query, after, hit and selection/window fields at the same level.
Read copies only matches[].hit and defaults offset to zero. Conversation optionally narrows the
first three operations and is required for the latter three. The model Tool does
not expose the separate human exact-source Advance/Search operations.
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
