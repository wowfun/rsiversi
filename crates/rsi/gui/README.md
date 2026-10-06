# rsi-gui

The Automation panel consumes negotiated, caller-scoped Automation API operations
through the same client connection as Session UI. It keeps a bounded page and one
selected attempt. Screenshots use a finite `automation_artifact` response outside
incremental frames and their retained baseline; only the open document panel
retains the returned PNG. Check verdicts and exploration claims are displayed
separately. Screenshot presentation uses a bounded PNG Blob URL, released on
replacement or panel closure, under the existing document CSP.
Cancel/Resume use fresh explicit request identities and never retry
an uncertain mutation; Resume sends the current rule revision. Session opening
uses the ordinary read-authorized attachment path; opening failures are displayed
in the panel, including when no conversation surface is available. The shared Web document also
renders this panel in Linux Desktop.

## Turn presentation

Each loaded transcript window owns a separate, bounded Turn index. Its revision
and upsert/remove patches reference stable block keys; reclassification never
changes a block's content revision. Classification is recomputed only for Turns
whose evidence or ordered window membership changed; text-only deltas reuse the
existing index without rebuilding classification vectors. The frame baseline
also shares its serialized Turn value and byte count until that index changes;
cache identity includes the index lifetime, not only its numeric wire revision. An incomplete window is explicitly partial
and never hides unknown content. Rust supplies explicit `running` and `foldable`
flags; document folding never depends on the wording of the status label. Entries disappear with their last loaded block.
Human steering and continuation, Agent, compaction and terminal boundary rows
remain in their actual positions.
Once the start of a Turn leaves the loaded window, its index stays partial for
that window's lifetime. Later deltas cannot manufacture the missing boundary;
attachment or return-to-live creates a new index from its newly loaded window.

Streaming text is a visible candidate. Only a completed Turn's latest successful
non-compaction model response without requested or subsequent Tools supplies an
answer; only a model Stop is successful without Tool calls. MaxTokens,
ContentFilter, Cancelled and Failed retain visible content without promoting it
to an answer or folding the completed Turn’s process. Direct Image output is an
answer of its own fact type. Missing evidence
produces a completion row, not an invented answer. Failed, cancelled, interrupted,
partially failed and budget-exhausted Turns retain content and expanded process.

The device-local detail mode is Compact, Standard (default), Detailed or Verbose.
Verbose includes the former Trajectory view. Compact collapses successful process
and summarizes running work; Standard shows the current work and completed
summaries; Detailed expands running steps; Verbose preserves full chronological
content. Expansion is bounded to the pane's loaded Turn window. Scroll updates
preserve the reading anchor unless the reader is following the end.

Native panes expose Export and `/export`, sharing the terminal argument parser.
Export bypasses model submission and is bound to the pane attachment generation.
The document pulls one bounded stream item at a time; detach, logout and explicit
cancel drop its stream. Filenames are hints, never service filesystem paths.
The [shared export contract](../session-export/README.md) owns artifact semantics.

Attention navigation uses the shared workbench's bounded current-owner view.
Opening a target revalidates its exact Native Session/Turn/request or External
connection/request identity. Native requests open the existing interaction detail;
external requests focus the matching permission group. The displayed source cut
is explicitly acknowledged per authenticated principal after attachment. Native
and External pane metadata both expose their actual backend capabilities.

A conversation surface selects either the native Session controller or a detached
external controller. Switching retires the old observation, never the Host-owned
external peer. External panes have text submission, exact peer permission choices,
observed history and explicit cancel/close/resume/load controls. Native model,
Goal, preset, terminal and extension controls are absent from external panes.
Every action and raw source delivery checks the surface attachment generation.
External catalog pages contain at most 64 observations and only configured endpoint
identities. The document cannot supply launch paths, environment or commands.

Resource previews carry a generation-local monotone revision. Clearing, starting
and completing a read advance it; document rendering compares this identity
without serializing the preview body. Draft reference rows similarly follow the
editor's reference revision and attachment identity.
Document retirement closes the file and reference dialogs with that attachment.

The finite reference-input bridge captures a durable source or reads a frozen
preview through the selected pane's actual Session handle. It accepts bounded
typed input and returns a descriptor or page; the document owns its insertion
into the persistent draft. Generation changes discard late UI delivery. Human
reference data is never interpreted as a slash command. Preparation includes
frozen references in the opaque request and admission rechecks CAS binding.

Passive model refresh updates descriptions on successful reads, including
changed capabilities for the same model. A transient read failure keeps the
prior metadata without producing a passive-refresh notice. Explicit
selection still validates current availability before mutation, and descriptions
for a different model never supply the current effort selector.

Model/effort controls submit the durable `model-selection` Session command through
one saved `CommandSubmission`; ordinary messages carry no model override. Unknown
selection outcomes retain the original invocation and refresh queries its receipt.
Pane projection follows the live selection domain. Effort choices come from the
selected model's described profile; a default remains an absent explicit choice.
Description reads confer no execution authority and stale attachment results are
not applied to a successor pane.

Model events carry their intent-checked purpose even in partial history pages.
Each request has one actual prepared model/effort, reported usage, duration and
failure summary, folded by the shared pure RequestPresentation. Backfill fills
missing metadata without inventing zero usage or substituting the current model.
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
Markdown and frame size checks use the shared bounded JSON counting writer,
without allocating an encoded buffer just to measure its length. This limits
aggregate frame expansion with the existing transcript budget.
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
[document draft contract](../../../apps/web/README.md). Image import returns
one canonical reference to the captured document record; it is durable independently
of submission and is never automatically replayed after reply loss.

Fresh Sessions created by Web use the default Agent preset. Their saved creation
intent has `agent_preset_id: null`; the document validates this producer contract
when admitting durable draft records.

Repeated workspace selection may reuse only the selected application's owned,
unpublished revision-zero draft with matching WorkspaceId/Header and unchanged
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
Workspace ordering separately reads the registry's atomic complete order seed.
Its 1,024-record / 128 KiB limit bounds the complete tree; oversized membership
preserves ordinary pages while device ordering pauses. Reaching the last page
never authorizes reconciliation of a partial membership.

Inputs enter a closed Rust command grammar with non-queued admission. The application
admits at most eight commands globally and one submission awaiting a receipt per
saved Session owner. Each pane retains at most 64 such owners and at most 1,024
owned pending message IDs per Session. The application retains no editable composer
mirror. Ordinary command documents are limited to 1 MiB plus their small envelope;
submission preparation and opaque execution have separate 8 MiB message and 32 KiB
command bounds. Encoded views use a separate 32 MiB reservation.
Preparation reads the current Header and validates full text/image input without
mutation. Dispatch revalidates the exact Session/Header and opaque typed request,
including absent sandbox, model and effort overrides for both next-turn and steer
delivery, then uses the shared controller.
Query-only reconciliation never sends input;
explicit message retry uses the controller's authoritative NotFound rule. Commands
are executed once and later only queried. Generic failures after dispatch remain
unknown, while pre-dispatch validation or admission rejection is explicit.
Definitive rejection releases an identity newly tracked for that dispatch. A later
rejection of an already tracked identity cannot erase an earlier uncertain attempt.
Stop carries the Turn identity from the displayed view and the exact attachment
generation. It cancels only that Turn, even if another Turn starts before delivery;
it preserves accepted pending messages, in-flight submissions and the editor.
Question and approval actions preserve exact
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

Opt-in `test-support` frame measurements record global materialization, bounded
JSON counting, top-level comparisons and the actual frame-stream lock duration.
Each calling thread retains only its latest synchronous frame sample. Default
builds contain no timing instrumentation. Detail generation fences asynchronous
reads; it is not a content revision suitable for caching unchanged global JSON.

The standard-product CI job explicitly lints this feature and exercises frame
reconstruction and materialization counts in ordinary submission tests. The larger
timing report remains ignored and uses the same case with additional samples.

Terminals use a finite request bridge bound to a pane's current Session
attachment. Rust records the issued terminal attachments and their controller
epochs. Closing the application, or closing or switching the pane, detaches those followers, without closing
the Session's shells. Terminal output bypasses renderer-frame acknowledgements;
the document cannot choose another Session or reuse a retired pane generation.
Teardown attempts all pane, card and surface cleanup even when one fails, and
reports that failure in the application shutdown result.

Rust also owns terminal input sequences, uncertain receipt reconciliation and
output cursors. The document forwards a bounded byte batch once, and acknowledges
an opaque output-page ticket only after xterm finishes parsing that page. A lost
reply returns the same retained page; it cannot advance the cursor twice. Input
uncertainty blocks further input until an explicit successful takeover. A definitive
stale-controller rejection immediately marks input read-only without receipt polling.
Exact terminal input `Capacity` refusals retry the same epoch, sequence and bytes
at most five times with 50/100/200/400 ms backoff, without receipt polling. Exhausted
refusal restores the unused sequence; input remains usable if no prefix was
accepted, while an already accepted prefix keeps the uncertainty latch closed
until takeover. Cancellation and unknown outcomes still block replay. Terminal
polling does not publish unrelated transcript frames.
Partial native writes reduce the next batch to the accepted prefix size. A full
acceptance doubles the next batch up to the input limit, so transient backpressure
does not leave a paste in one-byte round trips. Batch copies total at most three
times the original input size, even under repeated short writes; each refused
capacity attempt adds one bounded batch copy.

Terminal output reads have 32 independent admission slots, matching the PTY
provider's aggregate follower limit. They share command task ownership and
shutdown draining, but do not occupy the eight ordinary command slots. Terminal
writes have eight independent admission slots; lifecycle operations use ordinary
command admission. Output reads use bounded capacity retry on the Subscription
API lane. Detach always releases local follower state, even if the remote detach fails.

Workspace selection creates Sessions with default project instruction and skill
discovery; creation commands and draft reuse carry no workspace-trust setting.

History search uses the shared history API through the generation-fenced human
reference bridge. Freeze requests must name the pane's actual target Session;
source workspace and original evidence are independently checked by their owner.

GUI closed reviews show the exact request and send the selected stable action
and binding through the [question protocol](../../rsi-user-questions/protocol/README.md).

The pending-input view keeps one row per stable queue slot, including non-Human
sources without editing controls. Rust supplies current identity, delivery,
editable status and the exact displayed Turn eligible for conversion. Selecting
one row reads its complete immutable content on demand. Replace edits that
content while retaining images and references; it does not consume the ordinary
composer draft. Withdraw is pending-only. Convert to Steer carries the displayed
Turn identity. The existing frozen submission path also retains queue operation
envelopes, binds them to the original Session/Header, and resolves unknown replies
by operation lookup or an identical explicit retry. A reused operation identity
with a different frozen request is a terminal `rejected` settlement: the document
clears that queue intent while retaining the composer draft. A stale message is
instead an admitted domain rejection with a durable receipt and also settles the
intent. Both show readable reasons; neither offers endless identical retries.

Composer delivery actions are a Rust-owned projection bound to the acknowledged
pane attachment and an action revision. Preparation identifies that exact action;
a stale projection is rejected while the document retains its draft. It cannot
reinterpret a primary click using newly observed busy state. Frozen requests keep
their original delivery across retries. Direct Steer retains Agent semantics:
it enters the next available step or queues for a later Turn if the window closes.
Queue conversion and Stop retain their exact displayed Turn checks.
Discovered slash commands use their command catalog authority, not a message
delivery action; stale or absent delivery tickets do not block their preparation.
Queue preparation accepts only its queue payload. Mixed composer text, images or
references are rejected rather than silently discarded.

Directory dialogs and workspace registration explicitly retain the selected
execution location. Remote paths never pass through a Local registration default.
Directory dialogs use the composed shared picker client. Closing a list cancels
its exact ephemeral read identity; late results cannot replace a later selection.
A creation is sent once and its outcome remains explicit. Closing the application
drops read waiters; Host workers still retain their leases until actual I/O exits.

Cross-application preferences refresh starts after two seconds and backs off after
unchanged reads to four, eight, then sixteen seconds. A changed value or diagnostic
resets the delay to two seconds. Local saves publish immediately. Since
`SettingsAccess` has no change stream, each poll can be a remote Settings request;
idle peer changes can take sixteen seconds plus transport time to appear. Shutdown
cancels the watcher and an outstanding refresh waiter.


Stop admission requires that the exact pane generation and active Turn were
acknowledged by the document and remain current. A wire-supplied TurnId alone is
not display evidence. A changed theme or send preference does not invalidate
Stop; a changed attachment or active Turn does. Rejection sends no cancellation
to the Session and preserves submitted inputs and the composer draft.

Queue projection caches are bound to the queue lifetime/revision and displayed
Turn. Unrelated streamed facts reuse the sorted actions, immutable JSON and
encoded size. Changed slots or Turn bindings invalidate that cache; queue bytes
remain included in the same pane/frame budget and exact patch comparison.

Submission-owner retention follows explicit durable control evidence. Observed
message claim, discard or predecessor replacement, or an exact claimed/discarded
receipt, releases that pending ID. Absence from a pending snapshot cannot settle an
unknown dispatch;
receiving a completion on screen is not itself evidence. In-flight submission
permits and unresolved identities remain retained across attachment switches.
This permits continued navigation after 64 fully settled conversations while
preserving admission bounds and unknown-result reconciliation.

## Resource panel ownership

`PanelRegistry` retains independently cancellable resource views. A view ID is
monotone within one application and is distinct from a document pane or tab ID.
Every action/read must still match the current view ticket and the original
Session attachment. Moving, selecting or floating a tab does not replace that
view. Closing cancels and releases only that entry. Session detach closes every
entry from its exact attachment, and application shutdown drains all entries.
Settings reads/editing have an independent modal owner.

The registry admits at most 16 views and four floating views per Session, with
64 entries across the application. Capacity rejection preserves existing entries;
there is no authority eviction to make room. Restored or duplicated coordinates
must pass the ordinary open path and acquire a new view. Undo after close does
not revive a cancelled lease. Stored layouts never contain tickets, UI leases,
terminal writer grants or cancellation state.

A saved resource coordinate contains the stable bundle/surface name or exact
Session source, never a presentation reference, ticket or terminal attachment.
Reopening resolves that coordinate against the current attachment and obtains a
new presentation. Remote catalog pagination updates its existing view. A saved
remote surface is selected through the current Session export scope and goes
through the same Service selection validation as an ordinary catalog selection.
