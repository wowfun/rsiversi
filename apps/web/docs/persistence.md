# Document persistence

Composer access choices are bounded device preferences, keyed by connection
namespace and Session, with at most 128 entries. They contain only a sandbox mode;
eviction restores that Session's frozen default. Storage errors are reported and
do not apply a new choice. Submission preparation copies the selected mode into
the opaque request before its existing durable freeze; retry never rereads the
current preference. These preferences grant no capability or exemption from
tool approval, and do not change terminal policy or another client's messages.

IndexedDB schema 5 namespaces composer records by endpoint, real principal
(`local` or authenticated DeviceId), stable surface key and SessionId. Only this
schema is accepted; older schemas are refused without rewriting their records,
ledgers or version. Explicit discard is the separate recovery action. Rust still
checks every pending operation and immutable Header binding before reconciliation.
The origin-wide allowance is: 128 records, 4 MiB
editable text, 4 MiB pending text and 32 MiB opaque pending requests. Creating
another surface or connecting another principal does not increase that allowance.
The current editor record also retains at most four frozen conversation
references, charged to editable-text quota by their encoded descriptor size.
Captured descriptors keep their original target Header binding;
moving a Fresh draft to a different Header requires removing its references and
explicitly capturing them again. Capture failure, preview and removal preserve
the text editor and focus. Submission freezes text, images and references together;
retry uses the same opaque request and never captures again. Removing a reference
updates the local draft before best-effort remote release. A cleanup failure is
shown without restoring the removed reference or changing the draft's text.

Composer records live in IndexedDB under endpoint, authenticated principal, surface and
Session identity. Each record has a random incarnation, independent editable and
pending revisions, exact Header fingerprint, and optional Fresh creation intent.
All namespaces share the fixed origin allowance defined above. Each record permits
1 MiB text, eight canonical Media references and one unresolved submission.
No image bytes, object URLs or credentials are persisted. Counters and records
change in one transaction. Nonempty or pending records are never evicted.
Unresolved requests continue consuming these limits even if their Session becomes
unavailable. Enough retained requests can exhaust the pane's quota and block new
submissions across every namespace in that origin. There is no in-product discard
of uncertain requests: absence of a Session is not proof of non-execution.
Opening storage verifies records and aggregate counters in one bounded read
transaction; malformed or inconsistent storage fails closed without rewriting it.

Edits use incarnation/revision CAS. Conflicts keep the bounded local editor and
block sending until the user chooses the saved editor or explicitly replaces only
saved text/images. Neither choice alters the frozen request. Empty records can be
removed only with matching revisions and no pending request. Storage failures keep
local input with an explicit unsaved notice; failed pending persistence blocks
execution. There is no cross-tab execution lease or automatic owner takeover.
Within one pane, submission and reconciliation share synchronous admission before
any binding or persistence wait. Repeated input and automatic reconciliation
cannot run a second pending transition concurrently; failures release admission.
The local editor budget includes unsaved cached input. Its UTF-8 size is cached
per editor, so checking a keystroke does not encode every other draft again.

Rust prepares a full typed message or command invocation, including its original
identity, Session and Header fingerprint, before any mutation. The document keeps
that Rust JSON string opaque (including u64 revisions and argument member order).
It persists `prepared`, then CAS-transitions to `dispatching` and waits for the
transaction's completion before posting the execution call. Only the transition
winner may initially dispatch. Reloaded prepared input may explicitly execute or
cancel; dispatching or unknown input first queries its original identity. An
explicit message retry can resend the exact envelope only after authoritative
NotFound. Commands remain query-only after dispatch, including absent receipts.
A lost bridge response is unknown. Definitive pre-admission rejection returns to
prepared; settlement clears only the matching pending incarnation and identity,
and clears editable input only if its captured edit revision still matches.
Failed receipt persistence leaves pending input blocked, never allocating a new ID.
Completed receipts also arrive as opaque Rust JSON strings and are stored verbatim;
the document never decodes their sequence numbers through JavaScript numbers.
The stored `everDispatched` marker is conservative: it records that a dispatch
transition has ever begun, including attempts later proved not admitted. It is
not proof that a request reached the service. Once set, cancelling a prepared
request does not restore eligibility to recreate an expired Fresh Session.

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

Reference descriptors use the exact envelope-2 source/capture contract. Only
draft schema 5 is admitted; opening an older journal fails without modifying its
rows or database version. Unsupported reference metadata
is never translated into a new selection or silently removed from an uncertain
submission.
Changing History's query or source filters clears the displayed matches and their
pagination actions. Changing a source filter also drops discovery continuation;
pagination cannot silently reuse a previous query or workspace.

Device presentation uses IndexedDB `rsi.presentation` schema 3, separately scoped
by endpoint and authenticated principal. Three object stores have independent
origin-wide LRU budgets: preferences 4 KiB each / 64 records / 256 KiB; orders
128 KiB each / 16 records / 2 MiB; layouts 32 KiB each / 64 records / 2 MiB.
Record wrappers count toward each budget. A semantic intent reads the latest
revision and applies in the same read-write transaction. Writes preserve
unrelated records without rewriting them. Bucket accounting validates every
record's envelope and byte size; full content validation and Dock history replay
occur when that record is read or changed. Unread records confer no authority.
Each intent validates the freshly read history once. Its internally generated
result crosses the incremental state and record-budget checks without replaying
that history again. Independent `boundedRecords` inputs still receive full
content validation. LRU ordering is computed only when a write needs eviction.
An unchanged Dock intent still validates the selected document and bucket bounds,
but preserves its revision and LRU position without a write or notification.
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
transaction, without validating its old value or revision; unrelated records still
cross the bucket's envelope and byte limits. Reset does not alter Dock, navigation
preferences, orders or composer drafts. A failed load retains this repair action.
The schema-2 upgrade removes workbench layouts and preserves independently scoped
Dock records, preferences and orders. Earlier layout stores are discarded; no
compatibility conversion is retained. Version-change events close database owners
and show a reload notice. A blocked open exposes Retry after other windows close.
Neither
presentation records nor channel messages contain credentials, history or input.

The device layout record is version 4: navigation is expanded, rail or hidden;
widths clamp to 264 to 420 px (default 280) and the rail is 56 px. Below 1024 px,
automatic rail presentation does not overwrite the stored preference; mobile
uses a 280 px overlay. Resources start closed; their desktop width is a viewport
fraction, default 45%, clamped by 300 px, 70% and a 400 px conversation minimum.
Resize controls track viewport changes before applying keyboard or pointer deltas.
Below 768 px resources use fullscreen without changing the saved desktop mode.
Conversation width is null (automatic) or a safe integer of at least 640 CSS px.
Automatic width clamps 64% of the conversation column to 680–920px. Display width
is limited to the available column; viewport changes never rewrite the saved
preferred width. The composer allows 32px more than the transcript. Pointer and
keyboard handles share the same bounds; reset returns to automatic width.
Disconnect remains reachable through the command palette at every supported viewport width.
When the conversation column is at most 560 px wide, its tabs move into the
conversation menu so the global controls and conversation options remain distinct.
The compact menu also closes the selected additional conversation surface.
The layout owns the four detail modes and at most 16 recently used Workspace
expansion exceptions. Layout records retain the 32 KiB, 64-record, 2 MiB limits and
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

Session dock records share the layouts bucket, separately keyed by Session and
endpoint/principal. Semantic intents apply to the latest record in one transaction.
The adapter enforces two horizontal panes, 20 to 80% split, 16 tabs, four floats and
at most 64 undo entries within the record budget. Runtime view IDs, references and
terminal attachments never enter this record. Close/undo/reload resolve current
resource coordinates; terminal restore only attaches read-only. A failed save
leaves the committed layout intact and reports its failure.
Document resource opens carry a connection-local UUID and return an exact
view/open-revision receipt or an unavailable result. Frames echo the UUID;
neither reply ordering nor equal coordinates identify a pending tab. Duplicate
document tabs own distinct resources. A closed pending tab releases a late
receipt instead of adopting it into another tab.
Before saving a close, the connection reserves its cleanup responsibility. A
failed save preserves the old layout; a successful save publishes immediately,
independently of remote cleanup. Cleanup retains the original connection and
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
Resource host notifications describe membership and tab metadata; content-only
renderer frames retain the same external-store snapshot.

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
clears only the matching starting revision. Failure leaves a recoverable copy;
a late result cannot overwrite newer text or another endpoint's draft. This
handoff never invents a Session identity or bypasses submission receipts.

Starting text uses the separate `rsi.composer-starts` IndexedDB store. Its atomic
revision checks reject another document's conflicting write, with explicit use
saved/replace actions. Attachments and captured references require a Session;
the starting composer therefore exposes text and model configuration only.
