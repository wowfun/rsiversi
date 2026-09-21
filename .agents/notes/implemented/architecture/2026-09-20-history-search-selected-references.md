---
name: Rebuildable product history search and exact selected references
---

## Problem

Navigation searches bounded Session metadata, while references capture only the
latest bounded suffix. Neither can retrieve and freeze an older precise hit.
External observations have a different identity and replay epoch from native
Facts. A normal Fact page may materialize a 36 MiB record before a product can
apply its smaller scan budget.

## Decision

Keep metadata navigation distinct from a product-owned `ProductHistorySearch`.
Use an independently leased SQLite FTS5 cache outside the Agent Store root, with
native Header/cursor and external observation epoch/cursor checkpoints. Index
only direct human text, visible assistant text and explicitly exported Tool text;
exclude reasoning and provider request data. Treat FTS results as candidate source
coordinates, re-read through the source owner, and check workspace scope separately
for searching, reading and freezing a reference. Never manufacture native Facts
from external observations.

Introduce mechanical forward Store windows with pre-body byte admission. Index
batches inspect at most 256 records and 16 MiB; oversized rows are recorded as
coverage omissions, not silently skipped. Query pages retain at most 64 hits and
256 KiB. Two retained workers, a 1 GiB index ceiling, bounded SQL work and explicit
coverage/rebuild state keep a damaged or lagging cache from becoming authority.
Derived documents also stop before the next original at 8,192 fields or 64 MiB,
preventing short text fields from amplifying unbounded metadata. Retirement drains
dispatched operations before releasing the directory lease. Host startup awaits
enabled history and API owners before publishing a fixed capability snapshot;
otherwise a cache rebuild could permanently hide operations on that connection.

Reference envelope version 2 distinguishes native and observed source identities,
exact record/content kind and UTF-8 selection ranges, source cutoff and full-field
digest. The source owner rereads the selected original before freezing it into
immutable CAS. Preserve the existing two-worker/30-second reference budget,
1 MiB text and 64 KiB read windows. An old hit need not occur within the latest
1,024 Facts. Session format 17 and Store schema 23 reject their exact predecessors
without migration or modification of old database bytes.

## Alternatives considered

Putting FTS tables in the Agent Store would mix rebuildable product views with
durable execution authority and external protocol data. A second database under
the same Store root would obscure its exclusive lease. Reusing suffix capture
would silently replace the selected old hit with recent text. Trusting indexed
snippets would turn a corrupt or stale cache into user input authority. Generic
provider request indexing would expose reasoning and data outside the visible
conversation contract.

## Validation

Tests exercise pre-body materialization bounds on Memory and SQLite, old selected hits,
source growth, changed source identities and external replay epochs, corrupt-index
rebuild, incomplete coverage, cancellation retaining worker admission, and rejected
cross-workspace reads/references. Exact old Session and SQLite schemas must fail
without byte changes. Actual terminal, browser and Linux desktop interactions
search, open the original, choose a range and freeze it into a native draft.

## Consequences

Unicode lexical tokenization is not semantic search; the UI must state coverage
and show exact original text before selection. Large excluded records and cache
quota can leave permanent coverage gaps until the operator changes their inputs.
Source capture may become unavailable after external replay replaces an epoch;
already frozen references remain immutable and readable through their CAS owner.
