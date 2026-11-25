# Document persistence

Reloading a Web tab reopens its primary conversation after the same endpoint and
principal authenticate. The browser history entry holds the Session ID and its
identity namespace. It is updated when the attachment changes and cleared when
the user returns to a new conversation or disconnects. The Service checks
access on every reopen. A fresh tab starts at the new conversation surface.
The selected Chat or Trajectory view belongs to the authenticated conversation.
The document stores one device-local preference for each Session using
Trajectory. Selecting Chat removes the preference.

The Session panel docks by default when it fits. Hiding the docked panel stores
one browser-local preference shared by conversations. Showing it again clears
the preference. Opening or dismissing the temporary overlay uses no storage.

Unsubmitted ordinary question answers and the active question survive reopening
the conversation on the same browser. Progress belongs to the endpoint,
principal, attached Session, owning turn and request. A complete pending
observation removes progress for settled requests. Suspended or unopened
observations retain it. Untouched questions create no record. The question
remains answerable when the browser cannot retain its progress.

The composer displays the Host's current Session permission. The Host owner
changes it through the Session command. All windows submit new messages under
the Session policy. Older device-local access preferences are ignored. An
already prepared request retains its identity and its original arguments on retry.

IndexedDB schema 12 namespaces composer records by endpoint, real principal
(`local` or authenticated DeviceId), stable surface key and SessionId. The first
three components of that primary key select a pane's saved drafts in key order.
The primary key alone serves listing and single-record lookup. Its endpoint and
device IDs use their 16 bytes; a zero-length principal selects local access.
The value stores the draft content once and takes its scope from the key.
Upgrading schemas 7 through 11 preserves editor text, captures, pending identities
and their exact request bodies in one transaction. It drops completed receipt
copies and fences older writers. Other schemas are refused without rewriting
their records, ledgers or version. Refusal explains that the saved drafts remain on this device;
validation failures during asynchronous upgrade reads retain their specific reason
and abort the whole upgrade, including earlier cursor updates. Explicit discard
permanently deletes them. After a failed read, the window retains its input
and exposes recovery. Rust
checks every pending operation and immutable Header binding before reconciliation.
Ordinary Session text drafts use the available browser storage independently for
each Session, including when the same pane switches between them. Typing or
inserting a command checks the text and reference previews of the current draft.
Records containing images, references, outstanding releases or an
unresolved submission share an origin allowance of 128 records, 4 MiB editable
text, 4 MiB pending text and 32 MiB opaque pending requests. Settling the last
such obligation returns the record to ordinary storage.
Opening a Session reads its record without creating an empty durable draft. A
blank editor is materialized atomically on its first edit, freeze or cleanup
obligation. Concurrent first edits for the same Header share the record
created by the first writer. A stale editor fails its incarnation check after
explicit deletion and keeps local input. The recovery panel reads Session drafts
in pages of 32 and allows removal of saved empty records.
Restore drafts also permits confirmed removal of nonempty records without pending
requests, references or cleanup obligations. Removal checks the saved incarnation
and edit revision. The same panel exposes the existing confirmed site-wide discard
for quota recovery across namespaces, without revealing another principal's input.
The current editor record also retains at most four frozen conversation
references, charged to editable-text quota by their encoded descriptor size.
Captured descriptors keep their original target Header binding.
An explicit preset change on the same unpublished Session rebinds saved text
and images after the Host confirms its new Header. Pending submissions and
reference captures retain their original bindings until settled or released.
Moving a Fresh draft to a different Header requires removing its references and
explicitly capturing them again. Capture failure, preview and removal preserve
the text editor and focus. Submission freezes text, images and references together;
retry uses the same opaque request and never captures again. Attachment rows use
the complete target/snapshot capture identity, including its digest. Removal is
unavailable during submission preparation in the same pane. Removing a reference
updates the local draft immediately. Remote release follows a committed removal
and waits for every persisted editor and frozen-request owner in that record.
Explicit cleanup retry first saves local edits and reports unresolved conflicts or
in-flight ownership instead of silently succeeding. A failed release does not
prevent independent captures in the same outbox from being released.
An explicit retry during an automatic drain queues one coalesced retry pass;
an owned capture is never acknowledged as released.
If the original target no longer exists or its immutable Header has changed,
release is complete without contacting the replacement Session's capture owner.
Frozen requests expose exact target/snapshot capture identities separately from
their opaque envelope. Preparation temporarily retains those identities; freeze
checks that the saved editor still owns them. A conflicting removal cannot be
resurrected by replacing a stale editor.
Each record reserves at most sixteen distinct capture identities across editor,
frozen request and release obligations. Admission of new identities reserves their
eventual cleanup capacity; settlement and cancellation transfer ownership into
the outbox without increasing this total. Text edits remain available at capacity.
Both conversation and history reference pickers recheck this capacity before
transferring a temporary capture to the draft; a blocked preview retains its
capture until added or closed.
These bounded descriptors are metadata, separate from the text/envelope counters.
A capture added and removed before its first save is
retired separately after the coalesced edit commits; already-owned or queued
identities require no additional write. Cleanup is idempotent and acknowledged durably after
the remote response. Failed automatic cleanup waits for explicit retry or a new
binding/reconnection; text edits and subsequent saves do not retry that failure.
Release targets its original Session/Header/capture through the original endpoint
and principal, independently of the currently attached pane generation.
Runtime cleanup routing uses the same complete capture identity and authenticated
namespace as persistence. Cleanup retains an editor weakly; if the editor has
been evicted, retry reloads its saved record and checks ownership before release.
The connection is captured before that reload and must still be current before
release. Choosing saved input first retires all unsaved captures in one transaction
in the same record's durable outbox; a failed retirement preserves the local input
and queues none of those captures for release. Conflict
resolution briefly freezes that editor until ownership transfer and persistence
finish, so concurrent edits cannot reintroduce a capture already queued for release.
It does not retain the full text of every previously visited draft. Reload retries
cleanup. Failed cleanup leaves the obligation
visible without restoring removed input. Empty removal and Fresh transfer require
an empty outbox. Recovery lists offer cleanup without opening the original Session,
including an expired Fresh Session; recreation is offered only after its references
and release obligations are empty. Preview-only captures remain owned by their preview lifetime.

Composer records live in IndexedDB under endpoint, authenticated principal, surface and
Session identity. Each record has a random incarnation, independent editable and
pending revisions, exact Header fingerprint, and optional Fresh creation intent.
An ordinary welcome draft requests its staged preset or the configured default. An independent fork draft
retains the selected preset, source Session, completed Turn and history selection
(all, none, or a positive number of recent Turns). Moving a never-submitted draft
may change its new Session ID, but must preserve this creation intent; the Session
service reauthorizes the source when recreating it.
Each record permits
1 MiB text, eight canonical Media references and one unresolved submission.
No image bytes, object URLs or credentials are persisted. Counters and records
change in one transaction when the charged usage changes. Validation supplies the byte counts used for quota
accounting. The transaction edits IndexedDB's private read copy and retains the
previous capture owners until retirement is computed. Nonempty or pending records are never evicted.
Unresolved requests continue consuming these limits even if their Session becomes
unavailable. Enough retained requests can exhaust the pane's quota and block new
submissions across every namespace in that origin. Individual uncertain requests
cannot be discarded as unexecuted: absence of a Session is not proof of
non-execution. The explicit whole-database reset discards reconciliation identities,
warns that pending requests may already have run, and does not undo execution.
Opening storage verifies the indexed records with obligations and their aggregate
counters in one bounded read transaction. Direct reads and recovery pages validate
each selected record and its index membership before exposing it. A failed read
preserves the stored data.

Each window keeps its own Session editor. The latest committed edit supplies
the text, images and conversation references restored by a newly opened window.
Saving replaces those editable fields in the latest record. An established
editor cannot recreate a deleted record, change a replacement Header or restore a
capture retired by a peer. Frozen requests retain their identity and independent
revision across edits. Empty records can be
removed only with matching revisions and no pending request. Storage failures keep
local input with an explicit unsaved notice; failed pending persistence blocks
execution. There is no cross-tab execution lease or automatic owner takeover.
Within one pane, submission and reconciliation share synchronous admission before
any binding or persistence wait. Repeated input and automatic reconciliation
cannot run a second pending transition concurrently; failures release admission.
When a pane leaves a saved text draft, it can release the editor and read that
Session's record on return. Unsaved input and failed writes stay available for recovery.

Rust prepares a full typed message or command invocation, including its original
identity, Session and Header fingerprint, before any mutation. The document keeps
that Rust JSON string opaque (including u64 revisions and argument member order).
The first send records the envelope, its `dispatching` phase and the conservative
dispatch marker in one CAS transaction. Its winner waits for commit before
posting the execution call. A failed commit leaves the editor available.
Reloaded prepared input may explicitly execute with a fresh CAS claim or
cancel; dispatching or unknown input first queries its original identity. An
explicit message retry can resend the exact envelope only after authoritative
NotFound. Commands remain query-only after dispatch, including absent receipts.
A lost bridge response is unknown. A command whose arguments the Host definitively
rejects leaves its draft editable and retires its failed intent. Other definitive
pre-admission failures return to prepared. Settlement clears only the matching pending incarnation and identity,
and clears editable input only if its captured edit revision still matches.
The submitting window consumes its own captured input. A later save in another
window keeps its place as the restored draft. Reading pending or cleanup metadata
leaves the current window's visible input alone.
The composer shows an unconfirmed message beside its recovery actions. Delivery
details expose the saved identity and the current failure, and disappear when
the message is confirmed. The failure text stays with the current editor.
A detached no-argument command preserves the editor. A slash-triggered command
can carry a bounded consume span; successful settlement removes that span only
when its captured edit revision still matches, including after reload.
If saving the settlement fails, the pending input retains its original request ID
and body for reconciliation with the Host.
Rechecking an already unknown or definitively unadmitted result reads the saved
incarnation, pending revision and frozen request. An unchanged outcome keeps the
same revision and performs no write. The check returns the latest editor text
and detects another tab's settlement. Frozen request bodies compare as exact
strings, including sequence numbers outside JavaScript's safe integer range.
The Host retains completed receipts. Rust checks the typed outcome and returns a
completion status and any user-facing notice. A confirmed draft keeps no receipt copy.
The stored `everDispatched` marker is conservative: it records that a dispatch
transition has ever begun, including attempts later proved not admitted. It is
not proof that a request reached the service. Once set, cancelling a prepared
request does not restore eligibility to recreate an expired Fresh Session.
The first dispatch retires the Fresh creation parameters in the same transaction.

Restore drafts lists only the current authenticated namespace. Existing Sessions
must reopen with the same Header. A Fresh record can also reattach while its
original in-memory Session is still alive; the Worker reads its typed draft
snapshot before attempting durable history. A Fresh record with creation intent,
no pending request and proof that nothing was dispatched may explicitly create a new Session
if the original has expired; text and image references move only after that new
Session opens. Other unavailable Sessions retain their saved input without replay.
Recreation first saves the exact cached editor and prevents edits to that editor
during its transfer. A failed save blocks recreation and retains local input;
the recovery list offers the same explicit saved/local conflict choices as the
composer. It never replaces unsaved local input with the older persisted version.
Missing Media objects are shown as unavailable and require explicit removal or
reimport. An upload result belongs to the captured record incarnation even when
navigation or sign out occurs while the import is running.

Closing a surface or the document saves every retained editor, including inactive
conversations with a previous write failure. Surface close fences editing while
saving and restores input admission on failure. Ordinary submission saves only
its selected editor. Unavailable draft storage reports that diagnosis consistently.

Reference descriptors use the exact envelope-2 source/capture contract. Unsupported reference metadata
is never translated into a new selection or silently removed from an uncertain
submission.
Changing History's query or source filters clears the displayed matches and their
pagination actions. Changing a source filter also drops discovery continuation;
pagination cannot silently reuse a previous query or workspace.

Device presentation uses IndexedDB `rsi.presentation` schema 9, separately scoped
by endpoint and authenticated principal. Three object stores have independent
origin-wide LRU budgets: preferences 4 KiB each / 64 records / 256 KiB; orders
128 KiB each / 16 records / 2 MiB; layouts 32 KiB each / 64 records / 2 MiB.
Record wrappers count toward each budget. A semantic intent reads the latest
revision and applies in the same read-write transaction. Writes preserve
unrelated records without rewriting them. Updating an existing key reads just
that record and validates its envelope, content and replacement size. Adding a
key checks the bucket's envelopes, sizes and cardinality before eviction. Each
bucket's total limit equals its record limit multiplied by its maximum count.
At capacity, a new key evicts the least recently used record. Equal
timestamps are resolved by the lowest logical key.
A patch that requests already-selected shell or navigation settings preserves
the record revision and LRU position. Incoming
records still undergo the selected record's content and size checks.
Malformed selected content fails closed. Layout patches preserve
unrelated fields; expansion toggles preserve unrelated groups. Order moves use
complete membership and exact coordinate/pin/archive partitions; previous, next,
first and last operate across page boundaries. Updated mode clears that scope's
saved manual order. Reconciliation preserves absent IDs because the visible
catalog cannot distinguish a revoked member from a deleted one. Returning members
recover their saved position. At the 1024-ID bound, reconciliation rejects rather
than silently discarding positions; Updated mode explicitly resets the order.
Reading never writes or evicts. BroadcastChannel messages
only invalidate reads, and focus always rereads storage. Only committed values
are reported as saved; aborts retain the prior durable revision and show a notice.
An invalid layout falls back locally without opening or changing drafts. Explicit
Reset layout replaces only the selected principal's workbench record in a write
transaction, without validating its old value or revision. Creating a missing
workbench record follows the same admission as any new key. Reset does not alter Dock, navigation
preferences, orders or composer drafts. A failed load retains this repair action.
Obsolete Session dock snapshots are retired from the presentation database.
Version-change events close database owners and show a reload notice. A
blocked open exposes Retry after other windows close.
Neither
presentation records nor channel messages contain credentials, history or input.

The workbench layout stores conversation width, detail and workspace
expansion. Upgrades discard obsolete column dimensions
and desktop navigation mode while retaining other preferences, dock documents,
revisions and LRU positions. The [navigation column](presentation.md) belongs
to each window. A Session with no saved dock starts with resources closed.
On first opening, the document
chooses 45% of the viewport as its preferred width in CSS pixels. The document
retains that width across window resizes and panel toggles. Reload samples a new
initial width. The visible column respects the 300 px minimum, 70% maximum and
400 px conversation minimum. Resource width changes require no storage writes.
Below 768 px resources use fullscreen without changing the saved desktop mode.
Conversation width is null (automatic) or a safe integer of at least 640 CSS px.
Automatic width clamps 64% of the conversation column to 680–880px. A saved
width reserves 176px for the two gutters where space permits. On a narrow
screen the transcript fits the available column. Viewport changes never rewrite the saved
preferred width. The composer allows 32px more than the transcript. Pointer and
keyboard handles share the same bounds. Dragging previews at most once per
paint and commits on release. The 10px handles pass vertical wheel input to
the transcript. Reset returns to automatic width.
Disconnect remains reachable through the command palette at every supported viewport width.
When the conversation column is at most 560 px wide, its tabs move into the
conversation menu so the global controls and conversation options remain distinct.
The compact menu also closes the selected additional conversation surface.
The layout owns the four detail modes and at most 16 recently used Workspace
expansion choices. The first visit to a conversation reveals its workspace and
remembers the open group across archive filters and reloads. An existing choice,
including a collapse committed by another window, takes precedence over this
default. Layout records retain the 32 KiB, 64-record, 2 MiB limits and
never share a database or transaction with drafts.

Queue `rejected` settlements retire only the saved queue intent, without inventing
a durable receipt or clearing composer content. Unknown outcomes remain retained;
ordinary message submissions cannot use this terminal queue settlement.

Changing a navigation scope to manual, or clearing its order for updated mode,
commits its preference and order intents together across the two object stores.
A failed record or quota check aborts both; notification failure after commit does
not downgrade the durable receipt. The shared Host pin/archive metadata remains
outside this device transaction.
Automatic membership reconciliation reads the current preference in that same
transaction. It preserves the mode and changes saved membership only while that
scope is still manual. A stale restored view cannot undo Updated mode or refill
the order it cleared, including when another window made that choice.

Each window owns its Session resource dock. Opening, closing and focusing
tabs or changing the column's visibility at any width immediately changes
that window's layout. Collapsing the column keeps the selected Session's
floating panels visible and interactive. Resizing preserves whether the column is open.
Resource surfaces without a Session dock keep their visibility in memory. A localStorage
entry for each endpoint, authenticated principal and Session holds the latest
saved layout and ID counter. Other open windows continue using their own
layout. A fresh mount restores the last saved snapshot with an empty undo
history. A blocked or full storage area leaves the live layout usable.
A no-op leaves both local history and storage untouched.

Only the layout and coordinates enter the snapshot. Each window retains up to
64 undo entries in memory. The adapter allows two horizontal panes, 20 to 80%
split, 16 tabs and four floats. A snapshot is limited to 32,768 UTF-16 code units and checked on restore
before any resource opens. Live edits admit new coordinates and geometry
at the operation boundary and reuse the dock engine's validated layout.
Runtime view IDs, references and terminal
attachments stay with the current connection. Undo and reload resolve the
saved coordinates again. Terminal restoration attaches read-only.
Document resource opens carry a connection-local UUID and return an exact
view/open-revision receipt or an unavailable result. Frames echo the UUID;
neither reply ordering nor equal coordinates identify a pending tab. Duplicate
document tabs own distinct resources. A closed pending tab releases a late
receipt instead of adopting it into another tab.
Before closing a resource the connection reserves its cleanup responsibility.
The window can publish the change immediately after admission. Cleanup
retains the original connection and
resource incarnation, with one request in flight per identity and explicit retry
after failure when an exact cleanup identity is available. An uncertain opening
without an identity cannot be replayed: it shows a reconnect requirement, not a
Retry action. A matching frame can reconcile a resource opening receipt and binds
the originating tab even before its RPC settles. Leaving a Session dock commits
cleanup for its pending opens as well as its bound panels. Cancelled cleanup
reservations cannot subsequently dispatch. Resource projection accepts null
surfaces before a Session opens or after it closes; unchanged panel and surface
generation snapshots do not repeat reconciliation. Typed
terminal refusals before attachment allocation release the reserved responsibility.
Missing Worker application ownership is also a known refusal for resource opens
and terminal requests; it never implies an unknown allocation.
Retired-pane detach is owned by attachment shutdown, independently of current-pane
admission. Connection teardown failures appear through the connection's global
failure notice. Opens reserve their eventual cleanup before dispatch, sharing a
budget of 64 responsibilities globally and 16 per Session with attachments and
adopted-panel cleanup. Live opens retain that reservation; closing them needs no
new capacity. An uncertain allocation retains one responsibility until an exact
receipt or application teardown resolves it; it cannot be discarded to admit
more opens. Capacity refusal never discards an older responsibility. The cleanup
reservation stays inert until closure commits; observing an opening frame only
reconciles its identity and cannot authorize cleanup. The cleanup
alert is capped at 30% of the viewport (18 rem) and independently keyboard-scrollable,
keeping later Retry actions and the workbench reachable. Retirement
ends these ephemeral identities only after their original application closes.
Resource opens waiting for an ordinary RPC slot remain within the connection's
open-responsibility bounds. Slot release admits waiting opens one at a time in
FIFO order, reserving one ordinary slot for foreground commands. Draining wakes
queued opens with a known refusal. Layout
edits and cleanup do not await that queue. Retirement refuses undispatched opens.
Renderer and transport teardown both settle (including their bounded deadlines)
before resource retirement publishes their combined failure or success.
Terminal cleanup detaches followers and never terminates shells.
Asynchronous Dock setup belongs to one effect lifetime. Retirement prevents late
storage reads or reopen replies from publishing or installing subscriptions.
Resource host notifications publish membership, tab metadata and replacements of
the detail key or model used by React. Republishing unchanged host state retains
the same external-store snapshot; renderer-owned DOM changes alone do not replace it.

Retrying layout storage retains the current in-memory layout and replays unsaved
field and workspace intents before loading the stored result. Pending edits are
coalesced by field and by the 16 most recent workspace identities. Changing the
authenticated identity discards those local intents and loads that identity's layout.

## Starting a conversation

Before a Session exists, each endpoint/principal/surface has a bounded local
starting draft with its selected workspace and text. This record is independent
of model configuration and Session Header identities. Typing does not create a
Session. Model setup and workspace selection preserve the record. Sending first
creates the Session, durably copies the captured text into its bound draft, then
clears both text and workspace of the matching starting revision. Opening controls
with blank input does not materialize an empty saved Session draft. Failure leaves a recoverable copy;
a late result cannot overwrite newer text or another endpoint's draft. This
handoff never invents a Session identity or bypasses submission receipts.
Choosing a different workspace from an attached composer carries unsent text
and imported images from that window into the destination editor. Nonempty
source text replaces the destination text, and images follow any images
already there. Each Session keeps its captured conversation references and
their existing read and release bindings. Both Session records change in one browser transaction. Other
open windows keep their visible input. A pending submission stays with its
original Session. A failed transfer leaves the source input available.
Recovery captures the connection and namespace before storage reads. Closing its
panel, replacing the connection or resetting the pane retires that recovery;
each awaited transition rechecks ownership before creating, copying or clearing.
Session and starting-draft stores are read independently: a failed format cannot
hide healthy rows in the other format, and both failures remain visible.
Command insertion admits the editor change before publishing command selection
or textarea state. Record and pane quota rejection leave the displayed and
retained input identical.

Starting text uses the separate `rsi.composer-starts` IndexedDB store. Its atomic
revision checks reject another document's conflicting write, with explicit use
saved/replace actions. Attachments and captured references require a Session;
the starting composer therefore exposes text and model configuration only.

Pre-Session text lives in `rsi.composer-starts` schema 3, keyed by endpoint,
principal and surface, without a fabricated Session Header. Its independent
origin allowance is 128 records and 4 MiB text, with at most 1 MiB per draft.
Text, workspace selection, incarnation/revision and aggregate counters commit
atomically. Editors that observed a stored incarnation cannot recreate it after
deletion or replace another incarnation. A never-persisted blank editor's first
edit is fresh intent: it succeeds only if that key is currently absent, regardless
of intervening deletions. Concurrent first creations still conflict. No deletion
epoch or per-key tombstones are retained. Empty text without a workspace consumes
no record. The known schema-1 and schema-2 predecessors upgrade transactionally;
schema 2 only drops obsolete epoch metadata and preserves every record. Opening
the new schema retires older document writers through version-change handling.
Excess retained input remains readable and deletable but cannot grow beyond its
previous cost.
Recovery reads at most 32 starting records per page in the authenticated namespace.
The storage reader validates canonical namespace/surface keys and pagination
cursors before exposing rows; corrupt keys fail the read without deleting input.
Loading more appends rows and retains previous row state. A pending starting-draft
deletion has an explicit Cancel action; only confirmation removes the saved input.
Recovery distinguishes loading, failed reads and a successfully empty list. It
omits clean input already attached to the current pane, retains unsaved conflicts,
and states that opening a row switches this pane. Starting rows explicitly create
a new conversation here. Read-only input previews mount at most 4,096 UTF-16 code
units without splitting a surrogate pair; expanding a preview is explicit and
does not truncate persisted or recovered input. Destructive row actions have
row-specific accessible names, as do the read-only previews. A row shows its
saved text once and identifies whether it belongs to a Session or a starting
draft. When local text differs from saved input, both versions are labeled for
the recovery choice. Storage retirement
and claimed-command changes are announced through continuously mounted status
regions; the retirement explanation remains visible beside reset recovery actions.
Recovery storage errors appear before the rows, so partial recovery remains
visible without scrolling through the healthy database's records.
Known conversation titles label saved Session rows; unavailable titles fall back
to the Session identity so expired input remains distinguishable. Full draft
coordinates remain available in the row's title attribute. Starting surface names
use Main and Compare rather than their internal lowercase keys.

Whole-draft reset retires both database owners before requesting both deletions.
The lifecycle module is one external bootstrap asset shared by the bundled
document and the separately copied submission journal; both use the same owner.
Old editors cannot reopen storage; version changes and same-origin reset notices
retire other documents. Reload follows only successful deletion of both stores.
A blocked or partial failure reports the unfinished reset and permits explicit
retry. IndexedDB cannot atomically delete two databases, so reset never claims
success from one deletion. Presentation preferences are outside this reset.
