# rsi-navigation-api

Complete groups carry their execution coordinates and optional current registration
ID together. An unregistered coordinate has no registration ID. Clients consume
this mapping instead of deriving registration identities from paths.

Wire version 9 exposes each complete member's `account_position`: its zero-based
position in the caller-visible shared Workspace account. Positions are contiguous
within each group and independent of pin/archive state and later activity. Device
manual views may retain their own order; Workspace clients use this Host position.

The separate `attention` operations expose at most 136 current-owner conversation
candidates, each with at most 32 exact interaction targets. Pending interactions
sort before running work, unknown ownership and unread idle activity. Reading
positions contain lossless decimal epoch/sequence coordinates, are explicit
per-principal mutations and cannot acknowledge a future or replaced source cut.
Unknown ownership is never persisted as running state. A 256 KiB response cap
can truncate rows; clients display that fact. Polling never opens a Session or
starts a peer. The Host retains at most 4,096 read positions within 1 MiB.

The authenticated navigation API reads durable Session truth and edits only
Host-owned title/archive metadata. It never stops or deletes a Session. Titles
are at most 256 UTF-8 bytes and queries at most 128 bytes. A query scans at most
256 indexed activity rows and returns at most 64 matches. Its continuation carries an opaque process-local scan token, the exact query,
archive/workspace filter, Host generation and metadata revision. The owner keeps
at most 128 caller-bound cuts; an evicted, altered or foreign cursor requires an
explicit refresh. Tokens do not disclose skipped protected Session identities. A separate optional
last-visible activity key preserves client-side ordering checks; an empty filtered
page carries only the previous visible key. A protected newest key is omitted. Empty pages can have a continuation. Activity changes never invalidate a continuation. Rows that move ahead of the
cursor appear after refresh; a query is not a Store-wide snapshot. Each page
returns the newest activity key from the same read snapshot.

Grouping resolves the current WorkspaceId through an exact coordinate-key registry
lookup; equal paths on different machines remain distinct. Missing registrations remain unregistered; reads never create
workspaces. Metadata replacement uses an exact global revision and one complete
title/archive record. Clients do not replay writes after unknown outcomes.

The archive filter is tri-state: false selects active conversations, true selects
archived conversations, and null selects both. Default queries remain active-only;
pins, workspace groups and continuation identity use the same exact filter.

Navigation exposes immutable execution location and activity time.
Query, pinned and metadata replacement requests have a 4 KiB envelope bound;
summary batches have 32 KiB and coordinate-bearing order-seed requests 128 KiB.
The version-1 durable metadata document accepts a missing `pinned` field as false;
new records always write it. At most 64 records may be pinned. Archiving clears
pinning in the same revision CAS. Dedicated pinned discovery reads one Store
snapshot of all selected identities independently of ordinary continuation.
Listings authorize immutable Headers before exposing summaries, coordinates or
continuations. They do not decode transcript bodies. Unknown protected scopes
are omitted; exact protected summaries are null without View authority. Pins apply the same
query/archive/workspace filter. Order seeds carry the optional decimal `partition_revision` at which a Session entered its current pin or archive partition. Pins sort newest first, followed by unranked pins in descending activity and SessionId order. Archives sort oldest first, with unranked archives preceding them in SessionId order.
Missing activity remains disabled only when the retained Header authorizes the
read. Missing-Header metadata stays durable but is omitted from listings; it may
be explicitly cleared under revision CAS.
other read failures remain errors. Queries never clean up metadata. Ordinary
pages exclude pins. External rows must contain valid machine/path coordinates,
canonical nonzero decimal timestamps and strict descending activity keys.

Workspace filters explicitly select all, one registered identity, or unregistered
Headers. No matching row is not proof of exhaustion when a cursor remains.

Manual ordering reads an `order_seed` for all coordinates or one exact coordinate.
Search text does not remove members. The single Store membership snapshot includes
at most 1,024 identities; the complete wire seed including pin/archive partitions
must fit 128 KiB. `too_large` pauses manual mode without replacing saved order.
Malformed membership remains an error; only encoded-size overflow is downgraded
to that explicit capacity result when adding the wire envelope.
The response binds the immutable scope, Host epoch and metadata revision. Clients
reconcile complete membership first, then use `summaries` for at most 64 distinct
identities in requested order under that metadata revision. Missing rows remain
explicit null entries. No summary operation attaches a Session. Metadata changes
require a fresh seed; activity changes do not. Neither coordinates nor seeds grant
execution authority.

The seed dictionary contains complete execution coordinates; each member's group
index selects its exact workspace even in a flat view. Moves must preserve this
group and the shared pin/archive partition. Dictionary groups and IDs derive from
the same Store transaction; they are not assembled from later summary pages.

Each member also includes its exact decimal last-activity timestamp. On first manual
use, clients order all members by descending activity/identity before applying the
move; they never seed manual order from just the visible page.
