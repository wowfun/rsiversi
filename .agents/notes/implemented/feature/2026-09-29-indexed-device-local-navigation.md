---
name: Indexed activity navigation and device-local ordering
---

## Problem

Creation order does not identify the most recently active Session or provide one
workspace identity owner. Full manual ordering cannot be computed from a replaced page of
64 rows. Activity-driven cursor invalidation would starve historical pagination.

## Decision

Store owns canonical execution-coordinate and activity indexes, updated with the
canonical append transaction. A shared explicit record whitelist defines activity;
recovery, queue maintenance and compaction do not refresh it. Navigation owns
bounded indexed queries and one complete compact manual-order snapshot.
The GUI uses this projection for Session navigation and native history-picker
sources. It retains no second recent-Session catalog or pagination command:
fetching an unused list adds Service work and can fail an otherwise valid refresh.
The independent Session recent-list API remains available to its other consumers.
Its summary reuses the Store's Header/activity row, including both durable
watermarks from that row's read snapshot. The standard Session service applies
visibility to the typed rows and moves them to callers. A catalog does not need
to read the same metadata again after listing; that second read added worker and
SQLite work and could fail merely because a listed Session was deleted meanwhile.
This is a row-level observation, not a retained global catalog snapshot.

SSH visibility must enter that snapshot before limits and cursor selection.
Filtering the finished page leaks a hidden row through `newest`/continuation and
lets inaccessible records exhaust the 1024-member budget. The execution grant
owner therefore captures a bounded location selection and retains its live grant
gates for one finite enumeration. Store consumes only the mechanical selection,
using location expression indexes alongside the existing coordinate indexes.
Neither Store nor the serialized navigation cursor becomes an authorization owner.

Navigation uses activity/identity keyset paging with an explicit newer-activity indication.
Manual order admits a complete snapshot of at most 1024 Sessions or 128 KiB,
preserves hidden members, then paginates that order. A visibility-scoped catalog
cannot prove an absent identity was deleted, so automatic reconciliation retains
its position until explicit reset; the bounded record rejects overflow. Accessible move
actions reach across pages. Workspace trees partition by execution location.
Workspace owns a separate identity-ordered membership seed under one registry
snapshot, bounded to 1024 records and 128 KiB. Ordinary Workspace pages remain
replaceable views; neither their last-page cursor nor the currently visible rows
can establish complete membership for workspace-order reconciliation.

Host pin/archive metadata remains shared. Device preferences, workspace sibling
order and manual Session order belong to endpoint/principal-scoped IndexedDB.
Semantic intents apply to the latest revision in a single transaction; cross-tab
notifications do not supply correctness. Separate preference, order and Session
layout stores have independent admission and retention budgets.

Bucket capacity checks use record envelopes and encoded sizes. Only the selected
record needs semantic interpretation. A reopened Dock needs its current layout
and identity counter. Persisting its undo sequence would rewrite accumulated
operations and inverses on each gesture. The window owns that sequence and can
reuse it while the committed layout and counter match. A new window validates
the saved layout and starts its own sequence. A semantic intent checks its
result once before record accounting. The transaction writes changed records
and evicts the least recently used records when capacity requires it.

## Alternatives considered

Sorting loaded rows silently loses hidden members. A Host-wide manual order makes
one device's view change another device's preferences. Activity-fenced cursors
cannot finish while another Session is active. An unbounded full-history scan
does not provide a bounded navigation interface.
Accumulating every Workspace page would still mix registry snapshots and would
turn a presentation page into a second membership owner.
A separate metadata cache would add invalidation and retention to values already
available in the listing. Duplicating Session identity and coordinates in a second
summary envelope would add allocation without adding information.

## Consequences

Queries seek indexes without transcript reads or Session attachment. Recovery
does not change recency and continuous output does not invalidate ordinary paging.
Behavior tests cover 64/65 cross-page keyboard moves, hidden members, 1024/1025 and byte
limits, failed saves and independent concurrent window edits. Updated selection
does not mutate another device's order or Host metadata.

Manual order has an explicit capacity limit; exceeding it preserves the saved
order and visibly suspends manual mode. Live keyset results are not a historical
snapshot. This partially extends
[grouped pinned navigation](../../implemented/feature/2026-09-26-grouped-pinned-navigation.md).
