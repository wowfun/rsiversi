---
name: Complete pinned and workspace navigation
---

## Problem

A recent-page scan misses old pinned conversations and quiet workspaces. Attaching
a Session just to read its title metadata creates false activity. Missing attention
rows cannot establish idle state.

## Decision

Navigation reads Store-owned indexed summaries through a dedicated service operation. It bounds pins to
64 independent IDs, keeps normal group cursors separate, and preserves explicit
empty continuations. It retains durable metadata version 1 with missing pins false;
versions the changed wire contract. It uses attention as the only live-status source.

The Navigation writer records the revision of each new pin with its metadata.
A title edit or a new message can then leave the pin in place. The writer
serializes pin intents from multiple browsers and can acknowledge a duplicate
without cloning or persisting the document. Earlier records have no recoverable
pin time, so they keep their activity order until the user pins them again.

## Alternatives considered

Keeping pin order in a browser or a plugin would require reconciling a second
copy after native archive, purge or restart. The existing revision counter
already distinguishes successive pins without another clock or storage record.
Pin and archive membership are mutually exclusive, so the same revision records
entry into either partition. Title edits preserve that rank. Historical archive
order cannot be reconstructed from activity timestamps.

An application can ask several execution providers about active work or ask
them to stop. Holding the Navigation writer while those callbacks run would
block unrelated metadata edits. The writer commits the archive and pin removal
together and identifies the request that changed membership. That request can
notify providers after commit. Repeating the intent cannot cancel later work.

Copying Headers into metadata creates another durable source of truth. Increasing
the recent scan bound still misses old pins. Inferring idle from absence confuses
bounded observation with exhaustive state.

## Consequences

Old pins appear without attach side effects, group paging makes progress through
empty pages, archive unpins atomically, and missing Headers remain explicitly
removable. No transcript or model call is needed for an untitled row.


Each pinned refresh reads at most 64 exact summaries in one bounded Store snapshot,
without decoding Headers or transcript bodies. The
[indexed navigation decision](2026-09-29-indexed-device-local-navigation.md)
owns activity ordering and complete device-local membership. Concurrent metadata edits
invalidate all affected cursors and require an explicit refreshed view. They do
not silently load page one for every expanded group. Unchanged metadata leaves
continued pages at their current position; newest-page refreshes retain validated
pages during reads and keep their tickets when results are identical. Only a
failed group read invalidates that group's page. This preserves navigation position while keeping newly durable Sessions
visible without a transcript read.

Each expanded workspace retains a 64-entry page and its own continuation, with at
most 16 simultaneously loaded groups. Continuing replaces that bounded page; a
visible return-to-newest control keeps this limit explicit. Complete pins remain
a separate query and do not consume ordinary page positions.

Confirmed metadata replacement is never reported as failed merely because a
follow-up refresh races another revision. The view distinguishes saved state
from unavailable refresh and invalidates its cursors, rather than encouraging a
second write. Independent group refresh failures remain visible and do not stop
other groups from finishing.

Automatic reads have a finite deadline and yield their serialized owner to an
admitted user action. They do not reserve the user-operation slot across a group
fan-out. Read cancellation preserves pending invalidation; it never cancels an
admitted metadata write. Keeping the shared exclusive slot for background work
would make a slow service repeatedly reject otherwise valid navigation actions.
