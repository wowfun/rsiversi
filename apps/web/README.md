# Web document bridge

The standard renderer displays an optional `data.image` by reading its explicitly
declared PNG source in bounded 64 KiB windows. Its published renderer declares
the source capability, and reads start only after presentation activation.
The Session screenshot contract
admits exactly 1280x720 and at most 4 MiB. Replacing the model or disposing its
resource pane invalidates pending reads and releases the object URL. A new
presentation revision refreshes source reads even when its model is unchanged;
inline cards and resource details carry the current revision into renderer snapshots. The
Service source owner rechecks current Session authority for each window. Current
source failures appear in the screenshot figure; retired reads cannot publish an
image or error into a replacement snapshot.

The directory bridge preserves typed domain failures. A known validation or
pre-mutation filesystem rejection leaves folder creation editable. An unknown
transport or mutation outcome requires explicit parent readback before retry.
Confirmed creation refreshes the parent before showing the new child column.

Export uses a download-only Service Worker and a demand-driven MessagePort. It
never collects a whole artifact Blob or caches exported data. One-use download
identities bind the originating page; the Worker handles only its download path,
not application or authenticated API traffic. Rust owns Session authority and
stream validation. Disconnect and cancellation release the source stream.

Needs attention renders the shared Rust workbench's bounded activity projection.
It displays unknown ownership and truncation explicitly; request buttons send exact
targets back to Rust. The document does not infer execution from recent history,
persist running flags or maintain its own observation poller.

External conversation panes render the Rust controller's distinct identity,
observations and exact permission options. They have no native Goal, preset or
model controls. Close peer is explicit; switching panes only detaches observation.
Unknown prompt replies retain the entered text and disable sending that same draft
until the user edits it. The document never retries a prompt. Unsaved external
text is retained only for the current document connection, under a 16-draft / 1 MiB
limit; it does not enter the native durable submission ledger.

IndexedDB schema 4 namespaces composer records by endpoint, real principal
(`local` or authenticated DeviceId), stable surface key and SessionId. Upgrade
validates both old pane ledgers, migrates their records and commits one aggregate
ledger atomically. It preserves exact pending request strings, phases, receipts
and incarnations; a malformed record or quota mismatch aborts the entire upgrade.
Upgrade from schema 1 or 2 removes the obsolete workspace-trust field from
creation intents. Existing editor text, images, references and Header bindings
remain intact; it never rewrites the opaque Rust submission. A different service/Store version does not
make an old pending request replayable: Rust checks its operation and immutable
Header binding. Reconnect to the owning compatible service to reconcile it;
otherwise retain the old record for explicit recovery instead of replaying or
silently deleting it.
The origin-wide allowance remains the old two-pane aggregate: 128 records, 4 MiB
editable text, 4 MiB pending text and 32 MiB opaque pending requests. Creating
another surface or connecting another principal does not increase that allowance.
The current editor record also retains at most four frozen conversation
references, charged to editable-text quota by their encoded descriptor size.
Schema 1 and 2 upgrades add an empty reference list without changing pending
opaque requests. Captured descriptors keep their original target Header binding;
moving a Fresh draft to a different Header requires removing its references and
explicitly capturing them again. Capture failure, preview and removal preserve
the text editor and focus. Submission freezes text, images and references together;
retry uses the same opaque request and never captures again.

The first-party workbench uses one React 18.3.1 runtime for its workspace/session
navigation, selected detail-mode surface, composer and Session resource column.
The selected DSH SlotCore, React bindings and Button/StateDot primitives are
vendored with immutable provenance and MIT attribution in `vendor/dsh`. Their
Cordis service wrapper and store engine are excluded; RSI supplies bounded frame
sources and exact Session scope identities. Root slots own layout, and scoped
slots cannot render resources from another selected Session.

Bootstrap UI modules support Vite development HMR on loopback only. Production
bootstrap changes require a document restart. Independently admitted renderer
assets retain the existing candidate/drain/publication/ACK protocol; they are not
replaced through Vite HMR. The production build emits only flat self-contained
assets before publishing its renderer manifest. `pnpm-lock.yaml` pins the
complete dependency graph; builds use `pnpm install --frozen-lockfile` and never consume DSH's node_modules.

Development WASM preserves original, undemangled symbol names. After assembling
the complete bundle, the build runs the native WebAssets provider preflight and
reports success only after its default bundle and renderer graph are admitted.

These assets render the views of the ordinary [Rust Web application](../web-worker/README.md).
The document owns DOM nodes, focus and input delivery; the Dedicated Worker owns
the actual Rust Profile and application. One view crosses the bridge at a time,
acknowledged only after DOM rendering. Input delivery admits eight ordinary calls
and one separate lifecycle call at both bridge ends. Terminal reads and writes
have separate 32-slot and eight-slot lanes at both ends; frame acknowledgements use
neither lane. A failed draft flush cancels document close, keeps the current
editor and connection available for recovery, and never reports clean disconnect.
A completed close updates the document from its owner's Closed phase, including
when the transport returns an empty or null disconnect receipt.
Document close waits at most 30 seconds for each authentication, draft-save,
renderer-disposal, disconnect-receipt and transport-cleanup stage. A draft-save
deadline restores the current editor for recovery; a late save cannot continue
that close attempt. Cleanup deadlines remain observable failures and block
replacement even if the underlying operation completes later. Waiting never
asserts that native work was cancelled or that disconnect succeeded.
Native transport cleanup aborts its own HTTP request after 30 seconds and retains
that failed termination receipt for subsequent callers.
Native close cancellation also cancels that attempt's drain deadline; a later
close starts a new attempt after recovery. The native close-cancellation HTTP wait also expires after 30 seconds.
Failure to cancel native close retains the draft-save
guidance and fails the owning connection instead of escaping as a raw fetch error. Disconnect immediately fences ordinary admission and joins admitted
work and reply delivery before reporting clean shutdown. It does not gain capacity
by forgetting pending mutations. The document owns persistent composer drafts;
Worker frames never overwrite editable text or ordered image references. Dynamic content enters text
nodes or the Worker's restricted Markdown event stream, using only its closed
element set. HTML and remote Markdown images remain inert text.
Passive inline-card visibility updates pause during replacement. An in-flight
update rejected for the retiring attachment does not become a user action error;
the replacement frame supplies a fresh visibility observation.
Clearing a pane also invalidates the cached pending-interaction projection;
reopening an unchanged question or approval rebuilds its actionable controls.
Protected panes hide queue edits and pending question/approval actions. Frame
acknowledgement keeps both queue and stop bindings absent for those panes.
Pending controls carry their exact interaction owner and ID as DOM data; these
annotations identify a durable request across rendering and reconnects. Native
command bindings own the action inputs and authorization.
Approval details are identified by both the owning Session and request ID, so
switching between parent and child approvals also replaces their action bindings.

The standard renderer retains each button and its label node while its action,
label and value remain unchanged. Field and readiness refreshes preserve that
pointer target; changing an action descriptor retires its previous node and
handler. Handlers use the current fields, and the latest busy snapshot controls
availability even when an earlier invocation settles afterwards.

Composer actions wrap within the available width so every action, including Send,
remains reachable on narrow screens. The composer retains its complete input and
action rows when expanded extension state or attachments consume vertical space.
Its border does not become a clipping
viewport through flex shrink. Narrow workbenches scroll when their content needs
more height; transcript and extension content retain their own scrolling regions.
An unchanged composer action label retains its text node through draft-save
callbacks. Replacing the pointer target between native mouse down and up can
cancel WebKit click activation even when the button itself remains enabled.
The draft-status row reserves one line across ordinary save completion, so
removing the transient saving notice does not move a pressed composer control.

From the repository, run `pnpm -C apps/web install --frozen-lockfile --ignore-scripts`
and `pnpm -C apps/web build --debug`. The producer freezes the current source,
uses the installed wasm32 target and wasm-bindgen 0.2.127, and publishes a paired
native executable and complete asset bundle. The command reports the concrete
bundle directory; `target/rsi-app/current/rsi web` launches the latest managed
publication. Omit `--debug` for release. The optional absolute output directory
must be new and remains caller-owned.

A standalone `cargo build -p rsi-cli --bin rsi` does not have a Web build family.
Product Web entry rejects it before starting a Service. A paired executable checks
its assets even with `--assets`: bootstrap hashes and the Worker family must match.
Renderer generations remain independently replaceable through their existing
manifest and publication controls. Pairing is an artifact consistency check,
not distributor authentication.

`pnpm -C apps/web dev` supervises an isolated paired native/Worker upstream and
Vite's mutable document source overlay. This explicit development mode preserves
React hot updates and does not claim immutable document pairing. Rust and Worker
changes require rebuilding and restarting the supervisor. The Vite source root
excludes the sibling Worker package, repository state and build artifacts.

Open the configured Serve Web origin and paste the JSON receipt from
`rsi --profile devices -- register LABEL`. The receipt is used once and removed
from the form; only the endpoint identity is saved for an explicit reconnect
using the HttpOnly cookie. Sign out closes document input, flushes draft writes
and awaits renderer disposal before draining the Worker application and clearing
the cookie. Native window close uses the same document drain before requesting
Application teardown. If disconnect or the renderer connection fails, the document terminates
the failed Worker, closes modal details and clears its obsolete view bindings,
then returns to login for explicit reconnection. It does not acknowledge successful sign-out
or cookie removal. Sign out retains saved drafts and hides their namespace;
reconnection exposes only the authenticated endpoint and device namespace.
If draft storage cannot open or fails its integrity check, connection displays
an explicit storage-unavailable notice before a composer is opened. The connected
service remains accessible; sending requires successful draft persistence. This
connection warning accompanies later notices until disconnect; an empty Worker
notice cannot clear it.
Closing a browser tab cannot guarantee Rust cleanup; the service's
transport owners still bound and clean up their disconnected work.

The ordinary application Profile selects `rsi.application.service`,
`rsi.web.assets` with an explicit directory, and `rsi.application.serve-web`.
See the [Serve contract](../serve/README.md) for listener policy.

Presentation frames are snapshots or patches. The document applies a patch only
to its exact decimal base-frame ID, preserving unchanged pane data and stable
block identities. A mismatch leaves the current DOM intact and requests a
snapshot; a successful render acknowledges the resulting frame ID. The Worker
admits one frame at a time, with a 30-second acknowledgement deadline. Expiry
drains the connection before reporting failure. This presentation handshake never
owns Fact or control cursors.

Renderer modules export `mount(root, initialSnapshot, boundHost, abortSignal)` and
return asynchronous `update(snapshot)` and `dispose()` methods. One document mount
table admits at most 16 root, pane, sidebar or dialog slots. It resolves a nominal
renderer and exact schema only through the acquired generation catalog. Candidate
mounts finish in detached containers before replacing displayed roots; failed
mounts dispose their candidates and preserve the old generation. The frame is
acknowledged independently of renderer acceptance: an executable offer remains
pending while no slot exercises it. A failed first mount rejects that offer and
shows a resident unavailable placeholder for slots unsupported by the retained
generation; existing supported bindings stay usable; the Worker and ordinary Session remain
usable until another generation is published. Static-only offers need no module
execution. Renderer acceptance occurs only after an actual candidate mount.
A frame is acknowledged only after updates, DOM replacement and old disposal finish. A
failed disposal or update fails the document connection; a timeout never asserts
successful cleanup of arbitrary JavaScript.
During asynchronous updates, the bound host fences new input until the snapshot
and DOM have both been committed. Already admitted calls retain their original
host and model binding. A static-only catalog may display ordinary application
frames; requested renderer slots show an unavailable diagnostic until a catalog
can supply them. This does not retire the Worker or grant input authority.
Closing first aborts displayed and unfinished candidate bindings, then joins the
in-progress render and every asynchronous disposal. A renderer must observe its
abort signal during asynchronous setup. Closing has a 30-second deadline; expiry
reports incomplete cleanup and requires a page reload before further renderer
mounts. One document owner admits tables through `MountTable.open`, which waits
for successful closure of the previous table. Closing synchronously fences new
ownership. Any failed disposal or close deadline permanently blocks replacement
until a page reload, even if cleanup later finishes. Reconnect and late callbacks
retain their exact connection identity. Returning to login is not cleanup evidence.
One document connection owns transport, pending RPCs, authentication, mounts and
teardown. Async operations capture that owner before awaiting and cannot close or
dispatch through a replacement. UI callbacks additionally check current identity.
Authentication attaches only draft storage and its storage notice to the owner.
An unexpected authentication or storage-setup rejection fails that owner, rejects
its pending requests and starts its shared renderer and transport cleanup. The
authentication barrier always settles with presentation fenced; the failed owner
cannot present a waiting frame or admit further input.
Disconnect during Opening waits for authentication and storage setup, then drains
that same owner and publishes its disconnect receipt. Failure or replacement
settles the pending close through the owner's actual cleanup instead.
Disconnect closes admission synchronously once Ready; a failed draft save can restore Ready
only while the original connection is still current and Draining. Failure and close
settle pending requests once and share one teardown result. Replacement explicitly
retires the old owner and awaits renderer and transport cleanup before opening
another connection. Retirement does not wait for an abandoned draft flush; its
late completion remains fenced. `settled()` waits for a live drain, or for actual
cleanup after failure/retirement. Draft rejections retain a message even when
storage rejects with a primitive value.
Native transport cleanup requires a successful `/_failed` response. A rejected
response or transport error remains a failed teardown and blocks replacement;
the document must be restarted rather than opening a competing connection.
Attachment replacement disables that pane's input until its new view arrives;
typing cannot enter the retiring attachment between navigation and delivery.
Connection replacement resets navigation fences even when attachment selection
and generation are unchanged. Late retired navigation cannot retain or clear
the replacement's fence; input controls refresh with the replacement frame.
Single-editor draft flush waits for binding before selecting its editor. A
disconnect flush instead captures the existing editors before awaiting binding,
so it cannot save drafts owned by a replacement connection.
Transport failure also retires a Draining connection and rejects pending
receipts; draft flushing cannot suppress failure or revive a replaced owner.

Bound hosts expose only declared action/source membership and requested local
clipboard/focus capabilities. Clipboard text is limited to 64 KiB, uses browser
write permission on Web and a native write receipt on Desktop, and reports no
success after rejection. Their authority retires with the slot. Draft fields
are bounded document state, independent of renderer code and preserved only for
the same semantic binding. Modules are operator-admitted trusted same-origin code,
not a JavaScript sandbox; CSP and a closed manifest do not isolate hostile code.
The built-in standard dialog renderer is an ordinary dynamically imported module.

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

After the first bundle build, `node apps/web/renderers.mjs /absolute/bundle`
rebuilds only the standard renderer graph. Append `--watch` to observe its explicit
source file. The directory must belong to a running WebAssets configuration with
`watch = true` for publication. This renderer build never recompiles the Worker.
Changes to app.js, mounts.js, worker.js, styles.css, index.html or Worker Rust code
require a complete new bundle and application restart.

Browser ESM records remain cached for the document lifetime even after a renderer
releases its DOM and WASM instances. The bridge therefore admits at most 32
imported catalog revisions per document, including candidates whose imports fail.
Each revision admits only its catalog-declared renderer entry graphs; offers rejected
before any import consume no browser module records. Exhaustion
keeps the displayed generation and requires an explicit page reload for further
imports. Reconnecting the Worker does not reset this document budget. The server's
bundle leases and byte pool still release independently of this browser cache.

The document test command verifies every adapted DSH file against its recorded
SHA-256, including the retained license. CI runs both this command and TypeScript
checking before the product browser fixture.

Closing a surface or the document saves every retained editor, including inactive
conversations with a previous write failure. Surface close fences editing while
saving and restores input admission on failure. Ordinary submission saves only
its selected editor. Unavailable draft storage reports that diagnosis consistently.

The transcript reports at most four visible block keys per pane through a
coalesced, monotonically numbered replacement command. Inline contribution models
mount through the existing renderer table in the `pane` surface class. Scrolling,
history or Session replacement releases mounts and their server presentations;
late visibility commands cannot replace a newer selection. Inline actions and
source reads use the same exact snapshot tickets as details.

Visible-card hints retry once with the same sequence after a failed acknowledgement.
This idempotent presentation hint does not retry user mutations. Generation change
or disconnect stops the retry; a second failure remains visible to the user.

Registered Settings use schema-directed controls for bounded simple objects and
scalar/enum fields. Optional fields have explicit inclusion, and unsupported
schema branches use JSON without discarding unknown properties. Switching to JSON
preserves current form edits. The form rejects edited numbers, including nested
JSON fields, whose exact JSON
representation cannot survive JavaScript parsing. Use the full JSON editor for
those values; switching preserves the original numeric tokens from current form
edits, and the full editor forwards the original text to Rust.
The Settings owner still validates values and the exact displayed version
controls replacement.
Apply timing, validation failures and read-only state stay visible; credential
values use the separate setup operation. Plugin diagnostics use the shared
workbench's granted finite read and retain no Local Inspector authority.

Settings strings containing carriage returns use a JSON field, preserving CRLF
and lone CR through edits. Completion options retain their DOM identity across
unrelated frames; request, selection and diagnostic changes update the popup.

File-picker buttons use the same product action handler as the composer, so
synchronous and asynchronous failures reach the visible notice.

Session terminals render their independent bounded stream with pinned xterm
6.0.0 and fit addon 0.11.0, bundled into the existing application assets. GUI Rust
owns input sequencing, uncertain receipts and output cursors. The document
forwards bounded bytes and acknowledges pages after xterm has parsed them.

Terminal dynamic styles use constructed CSSOM sheets through a scoped document
override, with inert template placeholders and explicit disposal. The production
`style-src 'self'` policy is unchanged. `xterm-document.mjs` resolves the installed
ES module and validates it when Vite config loads; resolution also rejects a
different entry in development; production builds also require
the patched module to have been loaded. It checks the exact
dependency digest before correcting the 6.0.0 document-override precedence error;
dependency upgrades require reviewing this patch. No global DOM method is patched.

Terminal output reads retry an ambiguous bridge failure at most three times with the
same opaque acknowledgement. Rust retains cursor and page ownership; rendering
failures and admitted input writes are never retried by this document. Explicit
bridge admission backpressure retries with capped delays until detach, including
queued input known not to have been admitted. Exhausted ambiguous-read retries
leave a visible notice. Renderer initialization failure disposes its scoped styles
and terminal instead of leaving a partially mounted view.
Taking control applies the current viewport size even if the viewport has not
changed since the read-only attachment was opened.
The document input queue is a lazy 64-KiB ring including in-flight bytes. Each
accepted byte is copied into the ring and once into a dispatch batch; retries
retain that batch. A successful write alone removes its prefix. Overflow and
uncertain failure stop input without replay.
At document ingress, Worker JSON texts are decoded once and native decoded frames
are consumed directly. Renderer offers retain their byte-limit validation and
the existing acknowledgement and lease protocol.

Successful, exactly paired external delegation Tool cards remain visible in every
detail mode as compact navigation entries, including when their Turn process is
folded. Their navigation button is separate from source inspection. Verbose retains the full Tool text; opening
the card attaches the existing Host conversation and never submits another prompt.

Settings → Plugins also presents reviewed Host leaf management through the shared
Rust workbench. Configuration input crosses the bridge as text so exact JSON
numbers are parsed in Rust. The document retains only an ephemeral input draft;
source selection, grants, preparation, commits and receipt reconciliation stay
with their Rust owners. Saved source, directory durability and current runtime
application have distinct labels. Unknown writes expose the original receipt
query and do not offer a repeated save for that ticket.

Reference descriptors use the exact envelope-2 source/capture contract. Draft
schema 3, which stores obsolete reference envelopes, is not migrated: opening it
fails without modifying its rows or database version. Schema 1 and 2 drafts have
no references and retain their validated migration. Unsupported reference metadata
is never translated into a new selection or silently removed from an uncertain
submission.
Changing History's query or source filters clears the displayed matches and their
pagination actions. Changing a source filter also drops discovery continuation;
pagination cannot silently reuse a previous query or workspace.

External panes keep their switch guard through same-generation frames; only a
confirmed new binding clears it. Both buttons and keyboard submission respect it.

File previews use the [Files contribution contract](../../crates/rsi/session-files-ui/README.md#rich-file-previews).
SVG zoom uses the intrinsic dimensions validated from its encoded source, because
WebKit can report the responsive display size through `naturalWidth`.
The independently admitted `rsi.file-preview` renderer builds code and Markdown
DOM from bounded source windows. Its bundled Shiki grammars use the JavaScript
regex engine; large inputs retain plain source. HTML uses immutable sandbox
bootstrap documents whose response policies are owned by the asset transport.
The parent renderer is trusted same-origin application code, like other admitted
renderer modules; hostile same-origin code is outside the iframe isolation boundary.
A one-use MessagePort accepts only the immediate parent whose message Origin
matches the bootstrap document URL, transfers approved bytes and closes before
user scripts run. WebKitGTK custom-scheme responses do not reliably enforce
frame-ancestors; native navigation and this origin check remain required even
with the response CSP. An unauthorized embed cannot submit executable content. The
[preview boundary decision](../../.agents/notes/implemented/bug-fix/2026-09-22-preview-input-and-native-authority.md)
records the engine evidence and native authority rationale. Frame replacement discards that frame's state and authority. Previewing uses
no Media import. Video remains outside this preview renderer.

Typing `@name` offers Agent definitions with a human preview. Opening the completion
popup refreshes its catalog once; further keystrokes filter that snapshot. The explicit
file-path button opens the separate workspace picker. Both insert text only;
spawn resolution and definition refresh stay with the Agent contribution.

Renderer `mount` and `update` stage DOM while their bound host is inactive. An
optional synchronous `activate()` callback runs after the current snapshot and
DOM are committed and the host is active. Renderers may start asynchronous
source reads there, own their failures, and cancel them through the existing
abort/dispose lifetime. Staged and rejected renderers never receive activation;
mounting itself cannot invoke application authority.

An unchanged retained transcript keeps message nodes in place, including when its
omitted-history notice is present. Refreshes must not detach action controls
between pointer down and click. Unchanged transcript frames preserve the scroll
position even near the bottom; automatic tail scrolling follows changed content
or a newly bound conversation, so unrelated frames cannot move a pressed action.

The download worker serves an inert bootstrap frame under `/downloads/`. The
initiating page attests its exact frame window through its private MessagePort;
the worker then binds the one-use response to that controlled frame client. A
foreign page or a replayed URL cannot consume the stream.

Native export cancellation carries the token returned by `export_open` and uses
an independent eight-call control lane. Only a positively classified pre-admission
Busy response may be retried; cancellation delivery is awaited and a failure is
visible to the export caller unless saving already confirmed success. A late
cancellation delivery failure cannot turn that confirmed save into a failed export.

Closed human reviews display the exact request and explicit action choices.
Terminal clients accept a choice number followed by optional feedback; Web and
Desktop send the selected stable action and request binding. Free text cannot
approve a review. An answer receipt confirms delivery, not durable approval.

For the local product entrypoint, publish a paired generation, then launch it:

```bash
pnpm -C apps/web install --frozen-lockfile --ignore-scripts
pnpm -C apps/web build --debug
target/rsi-app/current/rsi web
```

The installed layout is `rsi` beside the complete `assets/` directory. Startup
does not build. `--assets ABSOLUTE_DIRECTORY` selects a bundle with the same
family and verified bootstrap bytes.
The local launch document clears its bootstrap fragment immediately, passes its
short-lived ticket to the Rust Worker once, and automatically connects through
the returned cookie identity. It never saves the ticket. Cookie recovery also
supports reloads and additional tabs. Manual device-receipt login remains
available for explicitly composed deployments.

## Presentation design

The [product design system](../../crates/rsi/docs/design-system.md) owns shared
principles. This document owns the GUI expression. System fonts include CJK
fallbacks; semantic CSS variables own light/dark palettes, geometry and states.
External CSS follows system appearance before authentication without inline
bootstrap scripts or a weaker CSP. Authenticated Rust preferences select system,
light or dark appearance and conversation font size. Refresh errors remain visible.

Navigation and resources can collapse and resize with pointer or keyboard controls.
On narrow screens navigation opens as a modal drawer. The command palette uses
Ctrl/Meta+K. Dialogs restore focus to their surviving trigger or composer. Standard
shows foldable Tool and Thinking summaries; Verbose exposes their details.
Expanding a summary retains its screen position and pauses automatic following.
Stop sends the displayed Turn identity with its attachment generation, preserving
queued input and the draft. It cannot cancel a newer Turn or a pending submission.

Device presentation uses IndexedDB `rsi.presentation` schema 2, separately scoped
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
An invalid layout can fall back locally without opening or changing drafts.
Schema-1 layout records upgrade transactionally into revisioned records. The
original object store is retained as `legacy-layouts-v1` for explicit recovery.
If its complete bounded collection cannot be validated, the new layouts store
starts empty; invalid legacy values never become active records or prevent later
opens. Neither
presentation records nor channel messages contain credentials, history or input.

Vendor provenance covers every vendored file except its manifest. License details
read the revision from that manifest. Product adapters own button geometry; the
pinned vendor sources retain their upstream licenses; each adaptation is recorded
with its source and adapted hashes in the provenance manifest.

Composer keys and the one-time legacy-key migration follow the
[client preferences contract](../../crates/rsi/client-preferences/README.md).
The sending-options menu exposes the displayed alternate delivery to touch and
keyboard users. Gesture handling captures the action revision before draft storage
flush; a later frame cannot reinterpret that action. Worker and Desktop confirm
successful frame presentation to GUI Rust before it admits that revision.

Workspace ancestry treats backslashes in absolute POSIX paths as literal filename
characters; Windows drive and UNC paths use their own separators.

### Conversation layout

The device layout record is version 3: navigation is expanded, rail or hidden;
widths clamp to 264–420 px (default 280) and the rail is 56 px. Below 1024 px,
automatic rail presentation does not overwrite the stored preference; mobile
uses a 280 px overlay. Resources start closed; their desktop width is a viewport
fraction, default 45%, clamped by 300 px, 70% and a 400 px conversation minimum.
Resize controls track viewport changes before applying keyboard or pointer deltas.
Below 768 px resources use fullscreen without changing the saved desktop mode.
Obsolete pixel layout records reset independently of composer drafts.
Sign out remains reachable in the header at every supported viewport width.
When the conversation column is at most 560 px wide, its tabs move into the
conversation menu so the global controls and conversation options remain distinct.
The compact menu also closes the selected additional conversation surface.
The layout owns the four detail modes and at most 16 recently used Workspace
expansion exceptions. Layout records retain the 32 KiB, 64-record, 2 MiB limits and
never share a database or transaction with drafts.

Transcript width is centered and the composer is 32 px wider, limited by the
available viewport. Empty and docked states retain the same composer node in
the same parent; CSS positions it. Process folding, history and streaming retain
a visible block anchor when reading away from the end. A bottom action resumes
following. Tool/source inspection remains available in all detail modes.

Paired Web output removes the WASM `name` custom section in both profiles.
Unoptimized Rust symbol names alone can exceed 32 MiB and exhaust the immutable
asset owner's 64 MiB aggregate budget. Cargo retains the original intermediate
WASM for symbol investigation; the served artifact retains executable code and
its build family. Resource budgets are unchanged.

Turn presentation writes block visibility and classification classes only when
their values change. Tail-following frames do not read individual message geometry. Reading anchors
are captured only away from the end and batch geometry reads before restoration;
unchanged patches retain block DOM state. Layout record admission measures each
existing record once and trims the least-recently-used prefix under the same
per-record and aggregate byte limits.

Existing installations with the legacy `enter_submit` field migrate to Enter to
send, including the old false default. Other preferences are retained. Choose
Enter to insert a newline again in Settings (`web.submit_key = mod_enter`); busy
submission defaults to Queue and exposes Steer in the sending menu.

Navigation restoration tracks only in-flight group loads (at most 16), releasing
its guard on success or failure. Automatic attempts are triggered by the desired
group set or a new navigation ticket, not by unrelated redraws; explicit expansion
can always retry. Row titles and native attention are indexed once per projection,
with one date formatter and shortest unique same-time ID prefixes.

Queue `rejected` settlements retire only the saved queue intent, without inventing
a durable receipt or clearing composer content. Unknown outcomes remain retained;
ordinary message submissions cannot use this terminal queue settlement.

Protected panes hide queue mutation controls and close the queue editor.
Queue rows reuse the last received array while its pane generation is unchanged.
Unrelated transcript patches do not serialize the queue for a comparison or
replace its controls. Layout persistence failures are visible; the current
connection can still use in-memory layout settings.
Layout reads and save replies cannot replace a newer local layout intent.
Notifications arriving during a refresh request another read, so a coalesced
invalidation cannot leave the document on an older saved layout.

Connection and preference alerts occupy their own flow rows. While an alert is
visible, the connected toolbar also reserves a row so it cannot overlap alert
text or block its controls, including at narrow viewport widths.

Changing a navigation scope to manual, or clearing its order for updated mode,
commits its preference and order intents together across the two object stores.
A failed record or quota check aborts both; notification failure after commit does
not downgrade the durable receipt. The shared Host pin/archive metadata remains
outside this device transaction.
Automatic membership reconciliation reads the current preference in that same
transaction. It preserves the mode and changes saved membership only while that
scope is still manual. A stale restored view cannot undo Updated mode or refill
the order it cleared, including when another window made that choice.

Navigation view and Session/workspace order are device preferences. Manual moves
use complete membership before reading 64 ordered summaries; first use initializes
from the full seed's activity order. Session moves stay within exact execution
coordinates and pin/archive partitions. Workspace moves stay within the same
execution location and immediate path parent. An incomplete workspace catalog
cannot prune or reorder saved membership. Keyboard Alt+Up/Down/Home/End and explicit
first/last controls share the same semantic move as drag/drop. A too-large Session
seed preserves saved order and displays updated results with a visible pause notice.

Navigation presentation schedules explicit commands and automatic summary reads
through one bounded 32-command lane. It waits for the prior command acknowledgment
before dispatching the next; obsolete summary tickets are discarded before dispatch.
Connection replacement invalidates queued work. Rust remains the ticket and
authorization owner, and the lane never retries a rejected or uncertain mutation.

The directory dialog selects Local or one caller-visible SSH target. Changing
location discards the old browsing window; every status, list, create and workspace
registration carries the selected location. SSH availability comes from current
Use admission, independently of Local configuration access. Folder creation is
single-shot, and an uncertain result requires explicit parent readback.

Docked resource tabs use the separately pinned DSH dockkit closure at
`4878cdabd87d4041bdaff61d04c966883b9fd07a`; earlier vendored files keep their
original revisions. Per-file provenance records source and adapted SHA-256.
RSI owns the layout adapter, resource authorities, persistence and Session
selection. The adapter admits two horizontal panes, a 20–80 percent split,
16 tabs and four floats per Session. Closing or moving the final tab out of a
pane merges the empty pane as part of the same undoable intent. Explicitly
splitting may leave an empty destination until the next move. At narrow widths,
a single displayed strip combines docked tabs without changing the saved tree.
Float rectangles are clamped to the current viewport for display; resizing the
window never persists these temporary bounds.
Global connection controls align with the conversation column, so dock tab
controls cannot cover sign-out or application close.
Settings has a fixed 188px navigation and independently scrolling options; its
800px shell uses the pinned panel radius and elevation, and becomes fullscreen
with section controls above the options below 768px.
Stable tab hosts retain visited terminal
followers across selection, movement, floating and fullscreen. Closing a tab
detaches; only explicit termination ends its shell. Restoring terminal coordinates
never creates a shell or silently takes its writer grant. The terminal roster
refreshes when its tab becomes active, so retained hidden UI cannot conceal
shells created since the prior observation.
Only a visible, nonzero terminal viewport with finite measured cell counts can
request a resize. Hiding a retained host never resizes its remote shell.

Session dock records share the layouts bucket, separately keyed by Session and
endpoint/principal. Semantic intents apply to the latest record in one transaction.
The adapter enforces two horizontal panes, 20–80% split, 16 tabs, four floats and
at most 64 undo entries within the record budget. Runtime view IDs, references and
terminal attachments never enter this record. Close/undo/reload resolve current
resource coordinates; terminal restore only attaches read-only. A failed save
leaves the committed layout intact and reports its failure.
An accepted reopen remains pending until its presentation frame arrives. Command
replies do not imply that the document has rendered that frame; late resources
bind to their pending tab instead of creating another durable tab.
Asynchronous Dock setup belongs to one effect lifetime. Retirement prevents late
storage reads or reopen replies from publishing or installing subscriptions.
Resource host notifications describe membership and tab metadata; content-only
renderer frames retain the same external-store snapshot.

Application extension restoration sends the ownerless application command;
Session extension restoration carries its current pane and generation. Narrow
resource-overlay visibility is transient and never writes the saved desktop
visibility, including when opening a resource tab. Collapsing docked panes on a
narrow viewport preserves the focused floating pane and the root dock selection.

Image previews capture the rendering connection. Detail close captures both its
connection and dialog identity before awaiting, so a late receipt cannot close a
replacement dialog. Global navigation actions capture their owner at invocation.

The document Node tests require Node 22.18 or later for direct TypeScript imports.

Document request admission counts each pending waiter once in its selected lane.
Replies (including failures), synchronous transport rejection and connection
retirement release that count; unknown or duplicate replies release no capacity.
