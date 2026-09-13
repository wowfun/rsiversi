# Web document bridge

IndexedDB schema 2 namespaces composer records by endpoint, real principal
(`local` or authenticated DeviceId), stable surface key and SessionId. Upgrade
validates both old pane ledgers, migrates their records and commits one aggregate
ledger atomically. It preserves exact pending request strings, phases, receipts
and incarnations; a malformed record or quota mismatch aborts the entire upgrade.
The origin-wide allowance remains the old two-pane aggregate: 128 records, 4 MiB
editable text, 4 MiB pending text and 32 MiB opaque pending requests. Creating
another surface or connecting another principal does not increase that allowance.

The first-party workbench uses one React 18.3.1 runtime for its workspace/session
navigation, selected Chat/Trajectory surface, composer and Session resource column.
The selected DSH SlotCore, React bindings and Button/StateDot primitives are
vendored with immutable provenance and MIT attribution in `vendor/dsh`. Their
Cordis service wrapper and store engine are excluded; RSI supplies bounded frame
sources and exact Session scope identities. Root slots own layout, and scoped
slots cannot render resources from another selected Session.

Bootstrap UI modules support Vite development HMR on loopback only. Production
bootstrap changes require a document restart. Independently admitted renderer
assets retain the existing candidate/drain/publication/ACK protocol; they are not
replaced through Vite HMR. The production build emits only flat self-contained
assets before publishing its renderer manifest. `package-lock.json` pins the
complete npm graph; builds use `npm ci` and never consume DSH's node_modules.

These assets render the views of the ordinary [Rust Web application](../../../crates/rsi/web/README.md).
The document owns DOM nodes, focus and input delivery; the Dedicated Worker owns
the actual Rust Profile and application. One view crosses the bridge at a time,
acknowledged only after DOM rendering. Input delivery admits eight ordinary calls
and one separate lifecycle call at both bridge ends; frame acknowledgements use
neither lane. A failed draft flush cancels document close, keeps the current
editor and connection available for recovery, and never reports clean disconnect.
Native close cancellation also cancels that attempt's drain deadline; a later
close starts a new attempt after recovery. Disconnect immediately fences ordinary admission and joins admitted
work and reply delivery before reporting clean shutdown. It does not gain capacity
by forgetting pending mutations. The document owns persistent composer drafts;
Worker frames never overwrite editable text or ordered image references. Dynamic content enters text
nodes or the Worker's restricted Markdown event stream, using only its closed
element set. HTML and remote Markdown images remain inert text.
Attachment replacement disables that pane's input until its new view arrives;
typing cannot enter the retiring attachment between navigation and delivery.
Clearing a pane also invalidates the cached pending-interaction projection;
reopening an unchanged question or approval rebuilds its actionable controls.
Approval details are identified by both the owning Session and request ID, so
switching between parent and child approvals also replaces their action bindings.

The composer retains its complete input and action rows when expanded extension
state or attachments consume vertical space. Its border does not become a clipping
viewport through flex shrink. Narrow workbenches scroll when their content needs
more height; transcript and extension content retain their own scrolling regions.
An unchanged composer action label retains its text node through draft-save
callbacks. Replacing the pointer target between native mouse down and up can
cancel WebKit click activation even when the button itself remains enabled.

From the repository, install the document toolchain with
`npm ci --ignore-scripts --prefix plugins/rsi/web`, then run
`node plugins/rsi/web/build.mjs /absolute/output/directory`.
The build uses the installed `wasm32-unknown-unknown` Rust target and a matching
`wasm-bindgen` executable (`RSI_WASM_BINDGEN` overrides its path). It copies only
the explicit bootstrap, mount bridge and admitted renderer assets consumed by WebAssetsFactory. The directory
must be empty, so a previous generation cannot be served as a mixed bundle.
The default WASM profile is release. Append `--dev` for a debug build with
development assertions; the build receipt names the selected profile.

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
See the [Serve contract](../../../crates/rsi/serve/README.md) for listener policy.

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

Bound hosts expose only declared action/source membership and requested local
clipboard/focus capabilities. Their authority retires with the slot. Draft fields
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

After the first bundle build, `node plugins/rsi/web/renderers.mjs /absolute/bundle`
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
