# rsi-gui

Conversation rows follow the final input order of each durable preparation.
The queue shows accepted submissions while waiting. A rejected or omitted
input contributes no conversation row. Text, image and reference details use
the Fact that owns each final block. A history page resolves unchanged inputs
outside its interval from the same Session.

Opening a conversation by ID uses the surface controller's Session attachment
for the initial history page and subsequent observations. Durable history starts
at one inspection cut. If inspection finds no history, the pane loads the live
draft. Publication during that read resumes from a fresh durable inspection.
Restoring saved input first checks its recorded Header against the target.
If the target cannot attach, the current pane stays open and the caller
receives the attachment failure recorded by the surface Profile.

Completed native Turns expose a manual fork action bound to the current pane
generation. Full, Last(n), and None create a fresh independent draft in the source
workspace and preset through the ordinary Session creation service. The document
flushes its editor before switching, and first input remains the publication point.
Forking resolves the source coordinates to the current registration at invocation;
opening history does not require a registration. Pane `workspace` contains the
immutable execution coordinates and their key, plus the current registration
or an explicit unregistered/unavailable state. Attachment and catalog refresh
resolve that registration by coordinate key, independently of catalog paging;
late observations cannot replace another attachment's presentation. Attachment
resolves only its own registration after retiring the prior owner; catalog refresh
resolves attached registrations before page reads so a failed page cannot hide a
registry outage. Catalog
`key` identifies the same coordinate-scoped history. Creation receipts retain
registration IDs only for draft reuse.

The model catalog retains one 64-item page shared by Settings and composers.
The directory picker receives its result in the initiating call. Listing folders,
checking location access and creating a child leave the workbench frame unchanged.
Next and first-page actions replace that page without changing the selected model.
Search applies to the current page; an off-page selected model remains visible.

Composer preparation accepts an optional typed sandbox mode for a next-Turn
message and freezes it with the request. Steering rejects sandbox overrides;
queue edits and uncertain retries retain their original options. Dispatch admits
that same bounded mode without rewriting it. The Agent enforces the execution
policy and its independent approval requirement as defined by the
[security contract](../../rsi-agent/docs/security.md). Pane metadata
exposes the immutable Header sandbox default; document preferences never mutate
that Header or imply platform isolation support.

An interaction refresh that confirms settlement removes its panel and succeeds.
Read failures preserve the panel and report the failed observation.

Interaction refresh uses the same pane, generation, owner and request binding
whether opened from attention or directly.

Reference release reacquires authorized access to the original Session independently
of the displayed pane. Its exact, non-reused capture ID identifies the pending
owner; a draft preset change may alter the Header without retiring its captures.
Absent Sessions and captures are idempotent releases. An old capture ID cannot
release a replacement owner's new capture, even when the Session ID is reused.

An initial discovery failure leaves the application open with a diagnostic and
an empty remote catalog; Refresh retries discovery. A composition without an API
client uses its supplied local services directly.
For the native composer the Worker projects one observed request with its pending
identity. A plan review precedes an ordinary question, which precedes an approval.
The last request of equal precedence in the observation takes the composer.
A user recovering an uncertain decision can read the latest details for that
request with one read from its owning service. Explicit resource inspection reads
current pending questions and approvals from the Session. Reinspection
reuses the panel for that exact attachment and interaction, advances its presentation
revision, and removes it when the request is no longer pending.
The pending field is null before the first successful interaction observation.
An empty observed list confirms that every request has settled in this attachment.
An accepted interaction snapshot also retires open details for requests that
another client settled. Reconnecting or failing to observe the Session keeps
the last confirmed controls and their decision locks.
Capability refresh replaces the last successful catalog only after discovery
succeeds. Frame visibility is derived once per catalog replacement. A temporary discovery failure leaves the previous feature visibility
and data intact. `try_command` separates refusal before application admission
from the result of admitted work; document adapters may enable an explicit retry
only for the former, without interpreting error text.

Settings dismissal is local and immediate. Each Settings window owns a monotone
revision carried as a decimal string in frames. An explicit catalog or namespace
open allocates a fresh window. Catalog paging retains it; namespace navigation
carries the current `window` identity and ignores stale navigation. Each read has
a separate internal fence.
`close_settings` cancels the exact window, including reads whose frames have not
yet rendered, through bounded control admission independent of ordinary commands;
a close for a previously closed window is a successful no-op. Resource panels use
`close_detail` with their explicit view and opening revision. Like Settings
dismissal, this synchronous close checks application admission but does not need
an ordinary command slot; it cancels reads even when all ordinary slots are held.
An old opening revision cannot close a replacement. Closing Settings cancels its
reads, not an already accepted save. The document fences dismissed revisions by
connection identity, so late frames or replies cannot reopen them or close a
newer Settings window.

Approval presentation keeps the action visible within the review and separates the
reason, execution location, working directory,
requested sandbox policy and prepared arguments. Requested policy is
not isolation evidence. Adapters bound argument and raw-request display windows
and explicitly mark omitted text; the decision binding retains the full request.
One pending decision disables both choices without retrying an unknown outcome.
Ordinary status labels use presentation vocabulary; exact durable coordinates
remain available in details. Composer keyboard instructions are visible and
programmatically associated with their editor.
Known machine-generated Turn reasons use plain presentation text; the complete
original request uses the same bounded Raw request window. Long words and paths wrap within
the review pane, including a 320 px viewport.

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

Chat shows one expandable retry row for a chain of model attempts. The row
counts down while the latest decision is waiting and discloses the selected
delay and failure reason. A start of the reserved effect in the same Turn
marks it started. Closing that Turn while it is waiting marks it cancelled.
Reopening the Session derives these states from the same recorded facts.
The row represents those failed requests in compact and standard Chat.
Verbose Chat and Trajectory retain each request and its original decision.

Native pane frames identify their Session through `kind`, `session`, `header` and
generation. Controls consume their owning command and contribution projections;
native frames do not duplicate identity or capability summaries. External frames
retain their distinct observed conversation identity and negotiated capabilities.

Each loaded transcript window owns a separate, bounded Turn index. Its revision
and upsert/remove patches reference stable block keys; reclassification never
changes a block's content revision. Classification is recomputed only for Turns
whose evidence or ordered window membership changed; text-only deltas reuse the
existing index without rebuilding classification vectors. The frame baseline
retains unchanged live blocks across snapshot updates. Settlement replaces their
transient text with durable source coordinates and invalidates only those blocks.
All surviving preview blocks of the settling model acquire its durable sequence
together before source insertion sorts the transcript. Their original content
order is the same during streaming, after settlement, and after a cold read.
It also shares its serialized Turn value and byte count until that index changes;
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
partially failed and MaxTokens Turns retain content and expanded process.
A preparation rejection displays a Blocked terminal row without inventing an
assistant response or a provider failure.

The device-local detail mode is Compact, Standard (default), Detailed or Verbose.
Verbose includes the former Trajectory view. Compact collapses successful process
and summarizes running work; Standard shows the current work and completed
summaries; Detailed expands running steps; Verbose preserves full chronological
content. Expansion is bounded to the pane's loaded Turn window. Scroll updates
preserve the reading anchor unless the reader is following the end.
Process blocks are the disclosure units, not a count of Agent Steps. Compact
omits process previews; Standard and Detailed show live detail and settled
reasoning previews. Detailed keeps running process bodies open. Normal completion
folds process in every mode except Verbose while preserving assistant replies.
Failed and incomplete Turns remain expanded. Mode changes retain message nodes.
For a settled Turn the projection identifies the displayed text blocks of its
last conversational model response containing prose. These positions address
the Turn's own bounded block list.

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
Preparation also returns exact target/snapshot identities for document ownership;
the document never decodes the opaque request to reconstruct them. The
[Web presentation contract](../../../apps/web/docs/presentation.md) owns confirmation
copy, announcements and document focus behavior.

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
Internal summary text and its request metadata stay outside the conversational
process classification. Summary text uses a Context compaction status block, never an Assistant
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

Each retained block owns its compact JSON projection. A snapshot, a patch and
a fresh presentation baseline share the same encoded block. Markdown events
are temporary during encoding. Their allocation capacity is bounded by eight
times the block's source bytes plus 16 KiB. Unavailable or over-budget
Markdown produces a cached plain-text block. Every visible block mutation
invalidates its projection; duplicate retained Facts do not. A live block that
outgrows its display window publishes its truncation marker with the same visible
text. Further output beyond that window reuses the encoded block. Earlier frames
keep their immutable bytes until released. Repeated controls preserve the history
anchor of unchanged previews and status blocks. Pane and UI revisions refresh metadata
without invalidating unchanged blocks. Frame construction borrows cached values,
and commits replacements only after successful output encoding. Generation
replacement and pane removal discard the corresponding baseline. Asset-only
changes still deliver a frame and participate in renderer mounting and ACK.

Assistant text may carry a restricted Markdown event stream alongside its exact
retained source. The application uses at most 64 KiB input, 4,096 events and 32 nested
elements per block. Encoded events may occupy at most four times source bytes
plus 16 KiB; exceeding any bound preserves the complete retained plain text.
Across the retained transcript, events therefore add at most 4.5 MiB of JSON.
Markdown and frame size checks use the shared bounded JSON counting writer,
without allocating an encoded buffer just to measure its length. This limits
aggregate frame expansion with the existing transcript budget.
Headings, paragraphs, lists, quotes, emphasis, GFM tables with closed cell
alignment, code and absolute HTTP(S) links are supported. An inline code span
whose entire value is an absolute HTTP(S) URL without whitespace keeps its
authored text and opens that URL. Inside another link it remains a code label.
Fenced code retains
only its first language token: at most 32 ASCII letters, digits, `_`, `+`, `-`,
`.` or `#`, normalized to lowercase. Other info strings do not enter the wire.
Tables and language labels share the existing event, depth, source, encoded and
retained-memory limits; over-budget tables remain plain text. Raw HTML is text.
In conversation Markdown, a run of two or more asterisks can open between a
CJK character and punctuation, and close between punctuation and a CJK
character. Symbols count as punctuation. Single asterisks and underscores
follow their ordinary delimiter rules.
An absolute HTTP(S) Markdown image carries its URL and alt text to the renderer.
The browser loads it with no referrer and offers a full-size preview. An
unavailable image falls back to its alt text or destination. Tool/file/source
contents keep their plain-text presentations.
Local Markdown image destinations become file-path events. After settlement
the renderer reads through the attached Session Files service, including local
absolute paths and parent paths. A read does not change the file picker's selection.
Inline and reference destinations preserve backslash separators in a Windows
path, including before punctuation. A path with both separator styles must
have a backslash before a letter or number. URL schemes of two or more characters keep
ordinary Markdown escaping, and character references decode in either form.
A standalone image paragraph can use an unquoted local image path containing
spaces when it begins with `./`, `../`, a POSIX root, or a Windows drive root.
An escaped example, a code span, or a larger paragraph containing this
space-separated form retains its ordinary Markdown interpretation.
Links to these same image paths carry a distinct file-link event so the
document can open the file from its label after settlement.
Ordinary local file links carry their decoded path and optional one-based
source line. The document resolves the link against the attached workspace
and passes a relative path to the Session Files document reader.
Completed tool blocks can supply the Files UI's bounded declaration to the
closing turn. A successful result must pair with its exact `present` intent,
including during result-first history replay.
Cards carry the description and the exact UTF-8 path, or its hex spelling for a
non-UTF-8 name. The observed length remains available in the tool result.
The document constructs only the closed element set with text nodes, and HTTP(S) links
open with no opener or referrer.
Parsing remains in Rust for Web Worker and Desktop. The document never reparses
transcript source with a JavaScript Markdown parser. Code presentation may add
bounded syntax tokens, language, wrap and copy controls without interpreting HTML.
Rendered code and its Copy action omit the line ending that separates the body
from the closing fence. Blank lines inside the code are preserved.
Dollar-delimited inline and display equations carry their TeX source as a
closed Markdown event. A fenced `math` block also carries display TeX. The
document can show the source while streaming or when rendering is unavailable.
Input composition suppresses keyboard submission until composition has ended.
Composer Enter behavior comes from the ordinary [client preferences](../client-preferences/README.md)
Settings contribution and is captured when the application connects.

The application imports browser-selected raster files through its composed Media
capability. One non-queued upload is admitted per application, at most
16 MiB source bytes per import. Composer ownership and durable retention belong to the
[document draft contract](../../../apps/web/docs/persistence.md). Image import returns
one canonical reference to the captured document record; it is durable independently
of submission and is never automatically replayed after reply loss.

Fresh Sessions created by Web use the staged Agent preset when one was selected.
Their saved creation intent records the explicit identity, or `agent_preset_id: null`
when creation used the deployment default. The document validates this producer contract
when admitting durable draft records.
Choosing a different preset on an attached unpublished draft preserves the Session
ID and its fork origin. The Session owner compares the current draft revision and
returns the selected Header. The GUI reattaches its presentation controllers to
that Header and rebuilds the completion catalog. References are bound to their
original target Header and must be released before selecting another preset.

Repeated new-conversation actions in a workspace may reuse the pane's current
empty draft or one it previously created and left untouched. Candidates must
still be unpublished at revision zero with matching WorkspaceId/Header and unchanged
agent/default-preset Settings versions. Only ordinary new-conversation drafts
capture a defaults stamp for reuse. Each stamp reads the independent Agent and
preset versions concurrently. New drafts capture matching stamps before and
after creation. Reattaching an eligible blank compares its creation stamp to
the current versions once. Recovery opens the handle whose Header was checked.
The document offers its exact binding only
after an empty text/image record has flushed and has no pending request or active
submission/upload. Rust independently checks draft ownership and admission. A
coalesced selection revision completes in-place reuse. Reopening a previous blank
binds a fresh presentation generation to its original SessionId. Only its origin
is retained with the pane's existing bounded submission records. The document
checks device-local saved input before offering a previous blank. Read uncertainty disables reuse; it never makes
Session creation invalid or reuses a historical/durable Session.

Detail previews read through the current exact-source ticket. A restored draft
thumbnail can read its canonical Media reference with the attached pane generation.
Replacing the attachment retires its thumbnail reads. Media owns canonical PNG validation and byte identity; arbitrary
URLs and filenames are never read capabilities. A Media preview or Session file
image read delivers its bytes or error to the caller without waking the workbench
frame publisher. Binary input and output travel separately from JSON views.
Source bytes are bounded before copying into WASM;
canonical response receive leases remain owned until the document transfer is
created. Two preview reads may run concurrently, independently of one upload.
Read slots and the GUI's 32 MiB canonical-byte admission remain with returned
immutable bytes through their final clone or slice. The document shares eight
objects / 32 MiB between cached URLs and active read reservations, with at most
eight waiting request descriptions. Only unreferenced LRU entries may be evicted;
capacity retained by displayed images produces an explicit retryable refusal.
Pending reads share only an exact connection, image and selection; completed
objects may be reused by another ticket on that connection. Display disposal
unpins its cache entry and drops queued interest. Blob conversion has a separate
32 MiB temporary allowance; transfer buffers are released after conversion.
Disconnect revokes all URLs and fences pending results.
The composer displays thumbnails for its draft images. Closing/replacing a display
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
The Host retains confirmed submission receipts. Rust checks their identities and
sequence numbers and sends the document the settlement status and any notice.

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
History is a bounded separate page, preserving live observation. When projection
limits evict the oldest visible Facts from a fetched page, the next read starts
before the oldest retained Fact so those earlier Facts remain reachable. A
single Fact retains its normal block and source-preview bounds. Workspace and
model catalog next-page actions replace their bounded page; they do not accumulate
an unbounded client catalog. Session navigation and native history-picker sources
use the navigation projection. GUI refresh does not fetch a second recent-Session
catalog, and the document command grammar has no recent-Session pagination action.
Workspace ordering separately reads the registry's atomic complete order seed.
Its 1,024-record / 128 KiB limit bounds the complete tree; oversized membership
preserves ordinary pages while device ordering pauses. Reaching the last page
never authorizes reconciliation of a partial membership.

Inputs enter a closed Rust command grammar with non-queued admission. The application
admits at most eight commands globally and one submission awaiting a receipt per
saved Session owner. Each pane retains at most 64 such owners and at most 1,024
owned pending message IDs per Session. Reopening reconciles accepted identities against the inspection's durable
control cut and pending set. Unknown outcomes and receipts newer than that cut
remain retained. This uses the existing inspection without per-message status reads.
The application retains no editable composer
mirror. Ordinary command documents are limited to 1 MiB plus their small envelope;
submission preparation and opaque execution have separate 8 MiB message and 32 KiB
command bounds. Encoded views use a separate 32 MiB reservation.
Preparation reads the current Header and validates full text/image input without
mutation. Dispatch revalidates the exact Session/Header and opaque typed request,
including absent sandbox, model and effort overrides for both next-turn and steer
delivery, then uses the shared controller.
Query-only reconciliation uses the Session controller to resume observation
when a receipt confirms the saved message. It never sends input;
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
omission is visible. After a batch merge, one scan from the newest block selects
the retained suffix within both capacities. A history page has the same projection bounds. The view
uses shared [conversation semantics](../conversation/README.md) for Tool outcome
classification and bounded JSON previews; it does not serialize complete large
arguments or result values before truncating them. The view keeps a Tool's name and argument preview when its result arrives, with distinct
intent and result Fact sequences. Arguments and results each occupy at most half the block; this also reserves
room for an intent loaded after its result. Clipping never removes its exact source.
Rejected Tools display their durable explanation in the same block, including
the default approval-denied message when no reason was supplied. Live observation
and history replay use the same explanation and retain the exact rejection source;
display does not consult a policy service or depend on a later model response.
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
Accepted controls update the queue. The conversation renders only final user messages
from Step input preparation, in their entered order. Older backfill cannot regress the current
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
re-encoding it. An observation whose projected values equal the baseline refreshes
its cached revisions without allocating a frame ID. An independently changed
renderer offer still requires a frame and acknowledgement, even when the GUI
patch is empty. Domain cursors advance when the Rust sink accepts updates.

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
validates retained block membership and captures its exact revision. Overlapping
visible sets reuse cards of the same revision without copying their displayed
text or source indexes. Eviction,
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

Resource panels expose a separate opening revision. Closing compares both the
view and opening revision atomically; absence or a newer incarnation is an
idempotent completion. Content updates do not change that identity. Document
`resource_open` requests carry a bounded UUID and a closed resource command,
retain ordinary command admission, and return the identity captured at allocation
or an explicit unavailable result. Their frames echo the request. Independent
document opens allocate distinct panels even for equal coordinates; ordinary
interaction reinspection may reuse a view with a new opening revision. These
identities are connection-local and confer no Session or action authority.
An opening request may own at most one live panel. Duplicate live correlation
IDs and a second allocation in the same command are refused before insertion;
the first receipt remains available for failure cleanup without poisoning the registry.

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

Detach is idempotent for an absent follower in the exact pane
generation, including after confirmed terminal close. Missing followers still
reject read, input, resize and takeover; cleanup never grants execution authority.

Terminal output reads have 32 independent admission slots, matching the PTY
provider's aggregate follower limit. They share command task ownership and
shutdown draining, but do not occupy the eight ordinary command slots. Terminal
writes have eight independent admission slots; lifecycle operations use ordinary
command admission. Output reads use bounded capacity retry on the Subscription
API lane. A failed exact detach retains its follower on a live pane for retry
until acknowledged or the terminal is known unavailable. Attachment retirement
attempts every follower and reports uncertain detach while draining other owners.
A committed pane switch succeeds even if retiring its previous attachment fails;
the cleanup failure is published separately. A late attach that loses its pane
reports any failure to detach the newly allocated follower.

Workspace selection creates Sessions with default project instruction and skill
discovery; creation commands and draft reuse carry no workspace-trust setting.

History search uses the shared history API through the generation-fenced human
reference bridge. Freeze requests must name the pane's actual target Session;
source workspace and original evidence are independently checked by their owner.

GUI closed reviews show the exact request and send the selected stable action
and binding through the [question protocol](../../rsi-user-questions/protocol/README.md).

Automation task details identify the linked Session by its available navigation
title or its Session ID. An archived target shows an explanation and disables
the link until the Host reports that the Session is restored.

The Web composer displays a single pending slot directly. Multiple slots share a
count button and initially stay collapsed. Expanding the list applies to the
current conversation until the queue empties. An editable single text block
opens in its row. Enter saves, Shift+Enter inserts a line, and Escape cancels.
Queue action buttons expose their names on focus and hover using the Web tooltip.
Structured inputs open the block editor. File blocks display their stored
filename and compact size. A claimed or replaced message retires
its editor. Only the latest selected message opens. A read admitted by the
acknowledged frame can finish during an unrelated frame update. The pending-input
view keeps one row per stable queue slot, including non-Human
sources without editing controls. Rust supplies current identity, delivery,
editable status and the exact displayed Turn eligible for conversion. Selecting
one row reads its complete immutable content on demand. Replace edits that
content while retaining files, images and references; it does not consume the ordinary
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

Workspace registration refreshes the catalog without creating a Session or
requiring model configuration. Creating a conversation remains a separate command.
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
Settings reads/editing have an independent modal owner. Editor projections include
the actual read version (scope identity and decimal revision string) for document
draft comparison; writes still require the current server-held ticket and version.
Terminal document failures distinguish typed pre-allocation refusal and confirmed
late-attachment self-detach from unknown outcomes. Detach for an issued, retired
pane generation acknowledges transfer to application-owned attachment shutdown;
the reply does not claim that shutdown has already succeeded. Shutdown failures
remain in the application diagnostic and retirement report. Such detach
it never acts on a replacement pane. Terminal cleanup follows the Terminals contract above. It does
not offer a retry through a replacement pane.

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

Model preview admission, cross-stream ordering and bounded candidate retention follow the shared [conversation contract](../conversation/README.md).

Incremental frame sections retain their owner identity, content revision, shared
JSON value and encoded size. Unchanged sections reuse that baseline without
materialization or byte recount. Section owners advance revisions synchronously
with mutation. Composite fields from preferences and details are captured under
one owner guard, including the revision used for those fields. Independent
feature projections may advance independently within a frame.
Aggregate watch wakeups and detail request generations are
not content keys. Composer action removal also advances its content revision.
The proposed section cache and action set become the baseline only after frame
encoding succeeds, so capacity failure cannot acknowledge unpublished actions.

`new_conversation` retires only the selected attachment and replaces it with an
empty surface, including when completed cleanup reports an error. That error remains
observable while the replacement accepts a new attachment. It does not create a
Session or require a model. The document
flushes the previous bound draft before issuing this command.

Turn presentation includes the typed terminal block identity. For complete,
successful turns with a retained answer, the conversation tab represents that
terminal once in the turn heading. Activity and incomplete or unsuccessful
turns retain the chronological terminal and its source details.

The Turn index carries durable block timestamps and boundary elapsed time. For a
complete, closed Turn it sums provider-reported usage once per conversational
request, including retries. A missing reading or an unrepresentable total
withholds the disclosure. Compaction requests have separate accounting.
When every contributing request has its prepared deployment and model, Turn
usage lists the distinct routes in their first-use order.
A generation-bound `metrics` command reads the existing
Session metrics API, preserves its watermark/completeness, and publishes only to
the still-current attachment. It does not infer whole-Session totals from loaded
blocks or block streaming progress on a metrics read.

Trusted document contributions read finite Session resources through
`read_session_resource`, bound to an exact pane generation. Request size is
bounded to 8 KiB; the existing controller owns read admission, response correlation
and retirement. A replaced attachment cannot receive a completed old read. This
path returns data without replacing the composer resource preview.
