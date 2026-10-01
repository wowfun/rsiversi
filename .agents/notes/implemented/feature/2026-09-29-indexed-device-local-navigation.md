---
name: Indexed activity navigation and device-local ordering
---

## Problem

Creation-ordered Header scans neither expose current activity nor provide one
workspace identity owner. Full manual ordering cannot be computed from a replaced page of
64 rows. Activity-driven cursor invalidation would starve historical pagination.

## Decision

Store owns canonical execution-coordinate and activity indexes, updated with the
canonical append transaction. A shared explicit record whitelist defines activity;
recovery, queue maintenance and compaction do not refresh it. Navigation owns
bounded indexed queries and one complete compact manual-order snapshot.

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

Bucket capacity checks validate envelopes and encoded sizes; each record's
content is interpreted only when it is read or edited. Replaying every unrelated
Session's Dock history for one layout intent multiplies semantic work without
establishing authority for that intent. Unread values remain opaque, cannot
authorize actions and retain their original records. The transaction writes the
replacement and actual LRU removals; it does not rewrite unrelated records.
The consuming read still validates complete history before any replay.
An intent's generated result goes directly to private record accounting after
the intent owner checks its resulting state. This removes a second full history
replay without trusting a previous read or another window's revision. The public
record-admission helper continues validating independently supplied content.

## Alternatives considered

Sorting loaded rows silently loses hidden members. A Host-wide manual order makes
one device's view change another device's preferences. Activity-fenced cursors
cannot finish while another Session is active. An unbounded full-history scan
does not provide a bounded navigation interface.
Accumulating every Workspace page would still mix registry snapshots and would
turn a presentation page into a second membership owner.

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
