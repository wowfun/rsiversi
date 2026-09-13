# rsi-gui

Model events carry their intent-checked purpose even in partial history pages.
Internal summary text uses a Context compaction status block, never an Assistant
answer block; raw source paging preserves its exact Fact provenance.

When composed, ordinary [workbench feature plugins](../workbench-ui/README.md)
supply navigation and redacted model-setup projections. Closed commands forward
to those owners; the GUI does not duplicate configuration policy or Store cursors.
Their independent change streams invalidate the shared frame projection.

An optional ordinary Application UI target exposes global contributions separately
from Session resource targets. Global selections have no fabricated surface or
Session generation; the same presentation lease, model revision, one-use action
ticket and awaited close own their authority. Closing a Session surface only
invalidates details actually bound to that surface.

When its selected API connection negotiates the UI operations, each pane can open
Service extensions. The application pages the remote catalog for that pane's actual
Session and retains one remote observation for the open detail. It derives scope,
presentation identity, source revision and one-use action tickets from retained
server responses; the document supplies only a displayed selection or action name.
Remote models use the same admitted document renderers as local models. Changing
details or detaching the pane closes the observation, without reconnect or replay.
Unknown mutation outcomes remain explicit; only a new server model can restore
input admission. This path can display newly installed Service UI contributions
without adding a linked Worker factory or rebuilding the application.

Markdown events are cached per retained block and invalidated only when its
source text changes. Eviction drops the cache with the block; unavailable or
over-budget Markdown retains an explicit cached plain-text result. Retained
Markdown capacity is limited to eight times each block's source bytes plus 512
bytes, within the existing 128-block / 1 MiB source budget.

The incremental frame baseline caches each block's immutable JSON and encoded
length by its own revision identity. Every visible block mutation replaces that
identity; duplicate retained Facts do not. Repeated controls preserve the history
anchor of unchanged previews and status blocks. Pane and UI revisions refresh metadata
without invalidating unchanged blocks. Frame construction borrows cached values,
and commits replacements only after successful output encoding. Generation
replacement and pane removal discard the corresponding baseline. Asset-only
changes still deliver a frame and participate in renderer mounting and ACK.

Assistant text may carry a restricted Markdown event stream alongside its exact
retained source. The application uses at most 64 KiB input, 4,096 events and 32 nested
elements per block. Encoded events may occupy at most four times source bytes
plus 256 bytes; exceeding any bound preserves the complete retained plain text.
This limits aggregate frame expansion with the existing transcript budget.
Headings, paragraphs, lists, quotes, emphasis, code and absolute HTTP(S) links
are supported. Raw HTML is text; Markdown images show their alt text without
fetching a URL. Tool/file/source contents keep their plain-text presentations.
The document constructs only the closed element set with text nodes, and links
open with no opener or referrer. Markdown does not grant script or media access.
Input composition suppresses keyboard submission until composition has ended.
Composer Enter behavior comes from the ordinary [client preferences](../client-preferences/README.md)
Settings contribution and is captured when the application connects.

The application imports browser-selected raster files through its composed Media
capability. One non-queued image operation is admitted per application, at most
16 MiB source bytes per import. Composer ownership and durable retention belong to the
[document draft contract](../../../plugins/rsi/web/README.md). Image import returns
one canonical reference to the captured document record; it is durable independently
of submission and is never automatically replayed after reply loss.

Fresh Sessions created by Web use the default Agent preset. Their saved creation
intent has `agent_preset_id: null`; the document validates this producer contract
when admitting durable draft records.

Repeated workspace selection may reuse only the selected application's owned,
unpublished revision-zero draft with matching WorkspaceId/trust/Header and unchanged
agent/default-preset Settings versions. The document offers its exact binding only
after an empty text/image record has flushed and has no pending request or active
submission/upload. Rust independently checks draft ownership and admission. A
coalesced selection revision completes this UI operation without replacing the
attachment generation or editor. Read uncertainty disables reuse; it never makes
Session creation invalid or reuses a historical/durable Session.

Preview reads require a current exact-source detail ticket, obtained by inspecting
a draft reference or a displayed source. Media owns canonical PNG validation and byte identity; arbitrary
URLs and filenames are never read capabilities. Binary input and output travel
separately from JSON views. Source bytes are bounded before copying into WASM;
canonical response receive leases remain owned until the document transfer is
created. The document retains at most eight object URLs / 32 MiB of canonical
bytes, evicts least-recently-used previews and revokes all URLs on disconnect.
Only a requested preview is decoded for display. Closing/replacing that display
fences late responses; application retirement cancels and drains image work.
These are presentation retention limits, not total browser decoder or RSS limits.

Each pane displays the latest complete extension-state snapshot, including fresh
drafts and idle control changes without a model request. Producer values and
failures are rendered as text. Snapshot replacement releases the prior retention;
attachment withdrawal releases the latest value. Projection-stream failure leaves
core history observation independent and marks extension state unavailable.

Fact, interaction and extension-state observation notices are independent.
An accepted update clears only its own stream's previous notice. Withdrawal
releases all retained renderer snapshots and projections, even if another owner
still holds the renderer handle; late deliveries cannot reacquire that retention.

Each conversation exposes its discovered Session commands and saved command
receipt. Registered slash names execute through the shared controller; unknown
names remain Human input for workspace skills. The document retains an unresolved opaque invocation across Worker replacement;
Rust owns command preparation, exact receipt matching and execution admission.
Submission settlements carry complete receipts as opaque JSON strings so the
document can persist exact u64 values without parsing their contents.

The private renderer factory takes null configuration. Pane identity and
generation belong to the application attachment and its input fences; renderer
activation retains no duplicate configuration for them.

The shared GUI application runs ordinary Rust Meta plugins, ordered Profiles,
domain clients and Session controllers in either a native Runtime or a Dedicated
Worker. Platform adapters supply connections and frame delivery; the document
thread renders views and forwards input. JavaScript persists editable input and frozen requests. Rust owns Session
execution, reconciliation, observation cursors, credentials and Profile state.

One authenticated connection supplies independent domain capabilities. The GUI
owns a keyed set of at most two logical conversation surfaces, initially `main`.
Surface keys contain 1..=32 lowercase ASCII letters, digits, underscores or
hyphens; they are stable document identities, independent of Session attachment.
Explicit add/close commands manage the set; a retiring key cannot be reused until
its existing controller/renderer Profile has drained. Cleanup failure is reported
but releases the retired key and its capacity after cleanup completes. Detail
retirement failure must not skip the controller/renderer cleanup. Frames carry a surface map
and patches identify exact keys. Membership changes require a full snapshot.
A Session-free Shell owns the ordinary renderer/controller child Profiles.
Each surface has its own model, transcript, history page and interactions. Composer
records persist independently in the document. Replacing one attachment does not
dispose the other. The Shell permits four surfaces to allow both old and replacement
generations during simultaneous switches; each pane admits one switch at a time.
History is a bounded separate page, preserving live observation. Workspace,
recent-Session and model catalog next-page actions replace their bounded page;
they do not accumulate an unbounded client catalog.

Inputs enter a closed Rust command grammar with non-queued admission. The application
admits at most eight commands globally and one submission awaiting a receipt per
saved Session owner. Each pane retains at most 64 such owners and at most 1,024
owned pending message IDs per Session. The application retains no editable composer
mirror. Ordinary command documents are limited to 1 MiB plus their small envelope;
submission preparation and opaque execution have separate 8 MiB message and 32 KiB
command bounds. Encoded views use a separate 32 MiB reservation.
Preparation reads the current Header and validates full text/image input without
mutation. Dispatch revalidates the exact Session/Header and opaque typed request,
including absent sandbox overrides and the prepared delivery/model pairing
(next-turn with a model, steer without one), then uses the shared controller.
Query-only reconciliation never sends input;
explicit message retry uses the controller's authoritative NotFound rule. Commands
are executed once and later only queried. Generic failures after dispatch remain
unknown, while pre-dispatch validation or admission rejection is explicit.
Definitive rejection releases an identity newly tracked for that dispatch. A later
rejection of an already tracked identity cannot erase an earlier uncertain attempt.
Cancellation inspects tracked input even before this attachment has observed its
first durable receipt, then prunes ownership to the authoritative pending set.
Cancellation targets the active Turn and this
pane's accepted pending messages. Question and approval actions preserve exact
request/owner identities. Settings writes preserve the read scope and revision.
The single detail view has its own generation: late settings reads and settled
answers cannot replace or close a newer view. History reads admit one operation
per pane; returning to live view fences a history result still in flight and resets
backward paging to the earliest Fact represented by the current live projection.
Workspace paths describe the selected server, not the browser filesystem.

Settings opens a bounded page of registered namespaces and shows the selected
owner's schema, default layer, application timing and sensitive-field markers.
Metadata and the editable snapshot must share a registration identity. The editor
retains the actual snapshot's version for CAS and reports provider writability.
Closing or replacing a Settings read cancels its local future; late success and
failure cannot replace another detail. Saving remains an admitted mutation under
the existing Settings owner even if the editor closes.
Pretty JSON output stops at 8 MiB before growing beyond the editor's rendering
bound. If indentation exceeds that bound, the editor uses complete compact JSON
from the Settings-validated value (at most 4 MiB). Values exceeding the command
input limit remain readable with saving disabled; no displayed prefix is writable.

Tool argument and result details use closed exact Fact sources, with decimal
string sequences preserved opaquely in JavaScript. Rust reads one exact Fact
through the shared controller and retains a 64 KiB UTF-8 field window. Paging
requires the current detail ticket; stale buttons cannot replace a newer view.
Closing or replacing details and replacing their pane cancel the old read.
Both success and failure are fenced by pane and detail generation; unavailable
sources are reported within their own detail. No full Fact or observation lease
is retained by a detail, and detail cancellation does not cancel the Turn.

Every retained non-Tool block exposes a source list. Opening it captures at most
the shared block-index bound in one metadata-only detail; each page displays at
most 64 references. Page tickets and pane generation prevent stale buttons from
replacing another detail. The captured list remains stable during streaming or
history eviction; a later exact read can still report unavailable. Only the page
is encoded into the document view. Media cards retain metadata and exact sources;
neither these identifiers nor a displayed URL confer Media read authority.
Source-list rows scroll inside their own bounded region, keeping the detail title,
close button and page controls visible across page replacement.

Each transcript retains at most 128 blocks, 128 KiB per block, 1 MiB of owned
text capacity and 512 KiB of metadata capacity;
omission is visible. A history page has the same projection bounds. The view
uses shared [conversation semantics](../conversation/README.md) for Tool outcome
classification and bounded JSON previews; it does not serialize complete large
arguments or result values before truncating them. The view keeps a Tool's name and argument preview when its result arrives, with distinct
intent and result Fact sequences. Arguments and results each occupy at most half the block; this also reserves
room for an intent loaded after its result. Clipping never removes its exact source.
History beginning after intent displays an explicitly missing intent rather than
inventing a Tool name or arguments. It also marks a page whose durable prefix
was not loaded, including the initial
attachment tail; a page can begin in the middle of streamed model output.
Rendering acknowledges an observation after its bounded Rust projection is retained.
Coalesced view notifications cannot advance or replace domain cursors. The DOM
bridge permits one view delivery at a time, acknowledged after rendering. Whole
valid API Facts and history pages remain possible transient allocations under
their independent domain/transport budgets; UI text limits are not RSS limits.

Text spans use the shared bounded exact-source index. Duplicate delivery is
ignored only while that source is retained; missing interior text is inserted in
source order. Evicting a span removes its source membership and byte-length
metadata together. A bounded single-field preview stays UTF-8 aligned; admitting
newer or older spans evicts from the opposite end and marks omitted content.
Accepted control previews reconcile to their entered Message fields, while direct
Turn input keeps a distinct identity. Older backfill cannot regress the current
Turn's status. The highest Fact sequence remains a cursor, not a duplicate set.

Login consumes a device registration receipt; the token is exchanged through
the existing same-origin HttpOnly cookie owner and is not persisted in browser
storage. Closing an application drains application-owned requests and surfaces and
does not stop the remote service. A WASM trap is a failed Worker, not proof of
clean Rust shutdown. Production uses TLS/H2; loopback HTTP is explicitly for
development transport debugging.


UI contributions use the independently composed `rsi-ui` registry. Each actual
pane Profile has a `rsi-session-ui` target depending on its exact controller.
The document renders generic contributed menus, cards, fields, forms and buttons;
Rust validates target and detail generations plus bundle-local action references.
Registry changes request a new view. Closing a contributed detail cancels its
read presentation; admitted actions stay tracked by their original owners.

The standard Worker composes the independent [Files UI](../session-files-ui/README.md)
contribution. Each actual pane supplies its own browser state over the connected
Files client, with an isolated Local mapping and no additional observer or pane.
The shared declarative inspector renders its directory and text/hex pages.
The independent [Agent tree UI](../session-tree-ui/README.md) uses that same
registry and detail slot for finite child-tree/history/source inspection.

The incremental presentation stream carries closed snapshot/patch frames with
canonical decimal frame IDs. A patch names its exact base frame, replaces only
changed top-level sections, and carries per-pane field changes plus stable block
upserts, removals and order. First delivery, pane-generation replacement and a
missing or mismatched base produce a complete snapshot. Each stream retains only
its latest projected baseline, bounded to 32 MiB of serialized JSON; output uses
the existing independent 32 MiB frame reservation. These are logical buffer
bounds, not RSS limits. Unaffected pane revisions reuse their projection without
re-encoding it. Domain cursors still advance when the Rust sink accepts updates.

The document acknowledges the exact frame only after successful DOM rendering.
A base mismatch requests a fresh snapshot. One pending frame and one acknowledgement
wait are retained; a 30-second stalled acknowledgement closes the application
connection and drains the application, requiring explicit reconnection. No queue
of frames or domain records is retained behind a stalled document. Application
withdrawal drops the baseline even while an external handle remains alive.

An ordinary platform plugin owns `web-assets.observe`, its application nonce and
its pending renderer offer. Asset changes wake the existing frame producer; they
do not start another document frame queue. The frame includes only validated
renderer admission metadata. The document completes asynchronous rendering and
old-module disposal before its acknowledgement settles the exact offer. Worker
withdrawal cancels observation and joins its task before connection cleanup.
Executable admission and DOM mounting grant no Session or credential authority.

Surface details hold the UI owner's asynchronous PresentationLease and an exact
SnapshotPin. Their watcher updates only captured model data; document rendering
performs no business reads. Closing or replacing the detail cancels its watcher,
joins lease cleanup, and fences escaped input. Source requests name the displayed
detail ticket and model-local source, with windows of at most 64 KiB. Actions name
only displayed membership; the application derives target identity and revision from
its retained snapshot. Standard block cards retain their independently bound
block action contract and are converted to the neutral standard model.

Each pane admits at most four visible inline block presentations (eight across
the two-pane application), leaving shared UI capacity for details and panels.
The document sends a monotonically numbered visible-key replacement; the GUI
validates retained block membership and captures its exact revision. Eviction,
history replacement, Session switch and plugin retirement cancel and drain the
same PresentationLease. Frames contain only captured models, never business
reads. Inline and detail cards use the same assets, action membership, source
paging and snapshot tickets. Tickets bind the presentation epoch and displayed
revision; a delayed response cannot populate a replacement block. Visibility is
a bounded read-admission hint, not authority to select a Session or source.
The sequence commits after new presentations are admitted. Failed admission can
retry the same sequence and reuse already opened cards. Retired-card cleanup
failures remain visible as notices while replacement cards continue opening.

The opt-in `projection_performance` test measures ten alternating-order runs of
16/64/128-block projection with Linux thread CPU accounting. It compares current
cached serialization with the source-preserved uncached serializer, first proving
identical JSON, and checks zero reparses of unchanged finalized blocks. Run alone
with `cargo test -p rsi-gui projection_performance -- --ignored --nocapture`.
This isolates projection caching; browser paint and whole desktop PSS require
separate actual-engine fixtures.
