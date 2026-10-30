# Renderer hosting

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

The standard renderer retains each button and its label node while its action,
label and value remain unchanged. Field and readiness refreshes preserve that
pointer target; changing an action descriptor retires its previous node and
handler. Handlers use the current fields, and the latest busy snapshot controls
availability even when an earlier invocation settles afterwards.

React creates stable empty renderer hosts and leaves their descendants to the mount
table. Loading/error diagnostics sit beside an inline host, so a new first-party
projection cannot erase renderer nodes before their owner's disposal settles.
Resource bodies use React portals into stable dock containers. Moving a tab moves
the container; it does not remount its content or change action authority. Resource
details bind their commands and image interests to the connection that published
them. Image interests are keyed by publishing connection and detail ticket; unrelated
frames retain the acquired URL. Replacing or retiring either releases the interest.
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
clipboard/focus capabilities. Clipboard text is limited to 64 KiB, uses browser
write permission on Web and a native write receipt on Desktop, and reports no
success after rejection. Their authority retires with the slot. Draft fields
are bounded document state, independent of renderer code and preserved only for
the same semantic binding. Modules are operator-admitted trusted same-origin code,
not a JavaScript sandbox; CSP and a closed manifest do not isolate hostile code.
The built-in standard dialog renderer is an ordinary dynamically imported module.

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

The transcript reports at most four visible block keys per pane through a
coalesced, monotonically numbered replacement command. Inline contribution models
mount through the existing renderer table in the `pane` surface class. Scrolling,
history or Session replacement releases mounts and their server presentations;
late visibility commands cannot replace a newer selection. Inline actions and
source reads use the same exact snapshot tickets as details.

Visible-card hints retry once with the same sequence after a failed acknowledgement.
This idempotent presentation hint does not retry user mutations. Generation change
or disconnect stops the retry; a second failure remains visible to the user.

The terminal footer displays its actual reported permissions. Local Windows
manual terminals show `Current user permissions`, separately from Agent access.
Terminal labels are shell-neutral; the browser platform does not select a shell.

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

File previews use the [Files contribution contract](../../../crates/rsi/session-files-ui/README.md#rich-file-previews).
SVG zoom uses the intrinsic dimensions validated from its encoded source, because
WebKit can report the responsive display size through `naturalWidth`.
The independently admitted `rsi.file-preview` renderer builds code and Markdown
DOM from bounded source windows. Its bundled Shiki grammars use the JavaScript
regex engine; large inputs retain plain source. Transcript code and file previews
share the fixed Rust/TypeScript/JavaScript/Python/JSON/YAML/TOML/Shell/HTML/CSS grammar
set. Highlighting admits at most 256 KiB UTF-8, 4,096 characters per line and
16,384 output tokens; unknown languages and larger inputs remain exact text.
Both themes share a per-call tokenization budget (500 ms by default, narrowed by
the transcript's document queue) and a 32,768 raw-token budget
(two theme passes), checked before and after each line. The pinned tokenizer's
per-line budget remains enabled at that same limit. These checks are cooperative: JavaScript
cannot interrupt an individual synchronous regex operation. Source and line caps
also apply; this is not a hard execution deadline. If either theme
stops early, the whole code block stays plain text; partial grammar state is never
displayed as completed highlighting. A document-owned grammar adapter observes
that completion flag because Shiki's high-level token API discards it.
Light/dark colors use the document appearance and do not require reparsing.
No grammar is fetched dynamically. HTML uses immutable sandbox
bootstrap documents whose response policies are owned by the asset transport.
The parent renderer is trusted same-origin application code, like other admitted
renderer modules; hostile same-origin code is outside the iframe isolation boundary.
A one-use MessagePort accepts only the immediate parent whose message Origin
matches the bootstrap document URL, transfers approved bytes and closes before
user scripts run. Browser and Desktop load these documents over HTTP; native
navigation and this origin check remain required alongside the response CSP. An unauthorized embed cannot submit executable content. The
[preview boundary decision](../../../.agents/notes/implemented/bug-fix/2026-09-22-preview-input-and-native-authority.md)
records the engine evidence and native authority rationale. Frame replacement discards that frame's state and authority. Previewing uses
no Media import. Video remains outside this preview renderer.

Renderer `mount` and `update` stage DOM while their bound host is inactive. An
optional synchronous `activate()` callback runs after the current snapshot and
DOM are committed and the host is active. Renderers may start asynchronous
source reads there, own their failures, and cancel them through the existing
abort/dispose lifetime. Staged and rejected renderers never receive activation;
mounting itself cannot invoke application authority.

Resource publications compare host, title, panel entry, content key and model
identity. Replacing a model must notify presentations even when its panel is
unchanged; identical publications retain the snapshot without serializing panel
metadata. A changed content key also resets detail-local presentation state.

Vendor provenance covers every vendored file except its manifest. License details
read the revision from that manifest. Product adapters own button geometry; the
pinned vendor sources retain their upstream licenses; each adaptation is recorded
with its source and adapted hashes in the provenance manifest.
