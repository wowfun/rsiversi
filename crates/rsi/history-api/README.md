# rsi-history-api

Progress reports `metadata_unavailable` when a metadata family or source Header
cannot be enumerated in this pass. Other families and known sources still advance;
the failed family is retried by a fresh discovery pass. A partial scan never claims
`discovery_complete`.

This wasm-safe version-2 contract exposes lexical history separately from
navigation's metadata search. Queries select Conversation, Workspace or
AccessibleHost: currently authorized registrations and locally saved history
on this Host, including offline SSH and disconnected ACP journals. Searching
never connects targets, starts peers, scans directories or registers workspaces.
Models derive their workspace and target from the actual Agent caller; the global
human API is not a model or Program tool. Hits carry precise source scope and
remain candidates.
Read and freeze reread the original through its source owner and reject a changed
Header, observed epoch, text digest, content coordinate or byte length.

Query reads the existing index. Discover scans saved metadata and advances one
source batch; Progress reads authorized coverage. A discovery pass consumes at
most 4096 returned workspace items and 4096 native/ACP metadata items. Each step
consumes at most 64 items and 2 MiB encoded metadata, then one source batch.
Retained identities and continuations fit 2 MiB. One second is a between-batch
scheduling budget; admitted work retains its permit through actual settlement
under the 30-second request deadline. End-of-scan is not an atomic Host snapshot.
Coverage separates discovery progress and limits from source indexed/observed
horizons, omissions and remaining work. Refresh starts another finite pass.
Rebuild deletes selected source entries; Reset accepts a Workspace range and rebuilds at most 64 admitted sources per
step, returning the next exclusive source key for interruptible progress. Query cursors bind query, scope, actual caller, authorized
source set, cache generation and content revision; changes require a fresh search
with Stale. Per-source Search cursors bind their query, exact source and cache
generation; each request independently rechecks caller authority. Discovery
continuations are opaque owner-retained tokens bound to the caller and range.
Unauthorized sources contribute no identity, preview or count.
Results contain at most 64 hits and
256 KiB, queries at most 256 bytes, and original reads at most 64 KiB of UTF-8.
Freeze binds the selected original to an independently authorized actual target
Session Header through caller-scoped admission. Authorized humans may read
protected history; capture follows [References' protection rules](../../rsi-agent/references/README.md). Human
selection transfers immutable data, never source authority.
