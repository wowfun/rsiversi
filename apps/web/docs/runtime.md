# Web document runtime

`node apps/web/build.mjs ABSOLUTE_EMPTY_DIRECTORY [--dev]` builds an independent
Web bundle. Document, Worker and WASM share one freshly generated internal
identity and a verified closed set of file hashes. It does not build a native
core or require a native build-family identity. The package manifest declares
the wire interfaces needed from the Host. Desktop bridge compatibility is
checked against the descriptor held with the admitted Web bootstrap.

The supervised development overlay uses the pinned Vite 6.4.3. Its owned HTTP
listener preserves port zero, proxy configuration retains Vite's supplied option
objects, and shutdown drains the version-specific dependency scan before closing.
Each overlay uses a private optimizer cache removed after settlement. Actual
listener/proxy and startup-cancellation tests cover these assumptions. The
development frame corpus and deadline metadata live with
[the control protocol](../../dev-control/README.md) and are checked by Rust and JS.
Only the supervisor's explicit configuration call admits a source server without
an already configured upstream. Environment flags cannot enable that exception.
The proxy preserves request authority; the native HTTP adapter fences all routes
before dispatch and owns browser authentication and CSRF policy.

Browser and Desktop both use the Dedicated Worker and ordinary authenticated
loopback HTTP/WebSocket transport. Desktop adds only the
[bounded native bridge](../../../crates/rsi/desktop-bridge/README.md) for clipboard,
streamed save-dialog export and draft-aware window closing. Its injected marker
selects conveniences; Tauri’s exact main-document URL capability owns authority.
The document embeds the bridge descriptor from its owning wire declaration.
Custom scheme locations do not select a second application runtime.

Export uses a download-only Service Worker and a demand-driven MessagePort. It
never collects a whole artifact Blob or caches exported data. One-use download
identities bind the originating page; the Worker handles only its download path,
not application or authenticated API traffic. Rust owns Session authority and
stream validation. Disconnect and cancellation release the source stream.

Bootstrap UI modules support Vite development HMR on loopback only. Production
bootstrap changes require a document restart. Independently admitted renderer
assets retain the existing candidate/drain/publication/ACK protocol; they are not
replaced through Vite HMR. The production build emits only flat self-contained
assets before publishing its renderer manifest. `pnpm-lock.yaml` pins the
complete dependency graph; builds use `pnpm install --frozen-lockfile` and never consume DSH's node_modules.

Every module under `src/runtime/` forces a full document reload in development,
as do bootstrap, Worker, connection, mount, persistence and shared-store owners.
Only presentation components use React refresh. This structural runtime boundary
also applies to newly added controllers, without extending a filename expression.
The development fixture observes a stable workspace-heading test identity rather
than depending on the heading's wording.

The TypeScript gate includes the actual `vite.config.mjs` with JavaScript type
checking enabled; pinned Node declarations and Vite plugin types cover its
build, CSP, proxy and reload configuration. The document's disconnect event
publishes settled resource accounting to the product browser fixtures.

After assembling the complete bundle, the build runs the native WebAssets provider preflight and
reports success only after its default bundle and renderer graph are admitted.

These assets render the views of the ordinary [Rust Web application](../../web-worker/README.md).
The document owns DOM nodes, focus and input delivery; the Dedicated Worker owns
the actual Rust Profile and application. One view crosses the bridge at a time,
acknowledged only after DOM rendering. Input delivery admits eight ordinary calls
and one separate lifecycle call at both bridge ends. Terminal reads and writes
have separate 32-slot and eight-slot lanes at both ends; frame acknowledgements use
neither lane. A failed draft flush cancels document close, keeps the current
editor and connection available for recovery, and never reports clean disconnect.
A completed close updates the document from its owner's Closed phase, including
when the transport returns an empty or null disconnect receipt.
Native close requests arriving during draining or after failure wait for that
owner's cleanup and acknowledge every queued attempt in arrival order. Each attempt is consumed before
awaiting the bridge, so rejection or a later attempt cannot reuse an old identity.
Document close waits at most 30 seconds for each authentication, draft-save,
renderer-disposal, disconnect-receipt and transport-cleanup stage. A draft-save
deadline restores the current editor for recovery; a late save cannot continue
that close attempt. Cleanup deadlines remain observable failures and block
replacement even if the underlying operation completes later. Waiting never
asserts that native work was cancelled or that disconnect succeeded.
Browser and Desktop share Worker RPC and frame acknowledgement. The
[Desktop adapter](../../desktop/README.md) separately owns the bounded native bridge
for window close, clipboard and save dialogs.
Native close cancellation also cancels that attempt's drain deadline; a later
close starts a new attempt after recovery. The native close attempt has a thirty-second watchdog.
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
Session projections and the composer subscribe to their runtime owner. React owns
first-party nodes; a frame stages typed data and commits those nodes before renderer
work can settle. Stop and queue bindings are absent during that work and are restored
only for the accepted generation. A native dialog leaves the top layer before focus
returns to its owner. Closing reference capture releases any late result; adding it
to the saved draft transfers that ownership. Retired callbacks cannot reopen a modal.
The Session owner retains editor and generation identity. Submission, picker and
completion modules implement its operations against that owner and the captured
connection; they do not create another Session lifetime or retain global state.
Element event bindings attach only to React's committed nodes and share one abort
scope with the Session mount.
Reset is valid before the first mount and after detachment, including a failed
React commit. Pane disposal clears staged authority and releases the same mount
scope used by React cleanup; projection and document shutdown delegate to it.
History search follows the same modal ownership: its controller owns queries,
coverage cursors and temporary frozen references; React renders the controls and
verified original. Changing the query or closing releases an unadopted reference,
including one returned after retirement. Adding transfers it to the current draft.
Pending controls carry their exact interaction owner and ID as DOM data; these
annotations identify a durable request across rendering and reconnects. Native
command bindings own the action inputs and authorization.
Approval details are identified by both the owning Session and request ID, so
switching between parent and child approvals also replaces their action bindings.
An unchanged interaction keeps its decision lock across new frame tickets and
command closures. Only an explicit successful pending-state refresh with a new
ticket releases that lock; a passive frame cannot resolve an unknown decision.
Prepared arguments and raw approval JSON each have a 256 KiB UTF-8 display budget
with an explicit truncation marker. Scrollable evidence is keyboard focusable.
Deny is the first decision, matching the TUI;
rendering never submits a decision automatically.

Local compilation has no fixed per-command deadline: a cold Windows release
build can exceed ten minutes in the Worker stage alone. CI owns its job deadlines;
the producer waits for the compiler and publishes only a complete successful bundle.

A standalone `cargo build -p rsi-cli --bin rsi` accepts independently installed Web
packages. Explicit `--assets` directories use the same internal bootstrap and
Worker verification described by the [asset owner](../../../crates/rsi/web-assets/README.md).

`rsi --watch web` supervises the configured source recipe and optional Vite
frontend with caller-selected homes and credentials. Vite serves mutable document
source after reporting its owned loopback socket through the
[development control protocol](../../dev-control/README.md). The admitted bundle
supplies Worker/WASM assets. React edits use HMR, bootstrap edits reload the
document, and admitted renderer updates publish through the existing listener.
Rust, Worker and Vite server configuration changes require restart. The Vite source
root excludes the sibling Worker package, repository state and build artifacts.
Ordinary product login uses the printed one-use launch link. This development
frontend is trusted application code with access to the selected real state;
independent XDG roots are appropriate for durable-format work.

For explicit Serve deployments and isolated Web fixtures, open the configured
origin and paste the JSON receipt from
`rsi devices register LABEL`. The receipt is used once and removed
from the form; only the endpoint identity is saved for an explicit reconnect
using the HttpOnly cookie. Disconnect closes document input, flushes draft writes
and awaits renderer disposal before draining the Worker application and clearing
the cookie. Native window close uses the same document drain before requesting
Application teardown. If disconnect or the renderer connection fails, the document terminates
the failed Worker, closes modal details and clears its obsolete view bindings,
then returns to login for explicit reconnection. It does not acknowledge successful disconnect
or cookie removal. Disconnect retains saved drafts and hides their namespace;
reconnection exposes only the authenticated endpoint and device namespace.
If draft storage cannot open or fails its integrity check, connection displays
an explicit storage-unavailable notice before a composer is opened. The connected
service remains accessible; sending requires successful draft persistence. This
connection warning accompanies later notices until disconnect; an empty Worker
notice cannot clear it.
The bridge owns separate transient and connection-storage notices. React alone
renders them as separate messages. React owns the Session surface, composer and
stable control nodes. The controller receives those hosts after commit and owns
draft/submission lifetimes. History and reference pickers, observed external
conversations, resource details and registered settings use the same React root.
Registered settings retain exact JSON text and their opened version while edited;
publication keeps controls locked until a fresh settings presentation arrives.
Focus is captured before disabling or replacing a settings control and restored
to an available control after commit. Independently admitted renderers
retain exclusive ownership of their host descendants and awaited disposal.
Keyboard composer focus remains outlined in forced colors. Observed navigation
activity has visible text in addition to its decorative color indicator, including
an explicit unknown state. Conversations absent from the bounded attention snapshot
have no activity row; absence does not establish that a conversation is idle. Completion
selection preserves option nodes; status messages are outside the listbox.
Current transcript visibility is observed per pane, retaining the last four
intersecting blocks in transcript order without per-block scroll geometry reads.
Inline-card selection waits for the initial observation of every current block;
an unobserved block is not evidence of an empty viewport. Removing all blocks or
disabling inline cards still clears the selection without waiting for observations.
Closing a browser tab cannot guarantee Rust cleanup; the service's
transport owners still bound and clean up their disconnected work.

The ordinary Serve application Profile selects `rsi.application.service` and
`rsi.application.serve`; `--assets ABSOLUTE_DIRECTORY` supplies the Web bundle.
See the [Serve contract](../../serve/README.md) for listener policy.

Presentation frames are snapshots or patches. The document applies a patch only
to its exact decimal base-frame ID, preserving unchanged pane data and stable
block identities. A mismatch leaves the current DOM intact and requests a
snapshot; a successful render acknowledges the resulting frame ID. The Worker
admits one frame at a time, with a 30-second acknowledgement deadline. Expiry
drains the connection before reporting failure. This presentation handshake never
owns Fact or control cursors.

`src/runtime/workbench.js` constructs the document owner with explicit transport,
mount-table and presentation ports. The production entry starts that owner once;
projection fixtures construct the same owner with isolated ports. Fixtures must
not remove imports or expose lexical variables by rewriting production source.

Each presentation has a ticket identified by the connection epoch and frame ID.
The React commit observer settles that exact ticket after the published snapshot
commits. Required host nodes must remain connected through renderer update and
disposal. Only then can the document enable frame-bound actions and return an
accepted frame. A swallowed slot error, missing host, unmount, replacement or
30-second presentation deadline fails the owner without a successful ACK. A
patch with an unavailable base still requests resynchronization. Frame tickets
never key React components or editor nodes.

The download worker serves an inert bootstrap frame under `/downloads/`. The
initiating page attests its exact frame window through its private MessagePort;
the worker then binds the one-use response to that controlled frame client. A
foreign page or a replayed URL cannot consume the stream.

Native export cancellation carries the token returned by the Desktop bridge's
`export_open` and uses its bounded native command lane. The Worker owns source
export events; the native adapter owns the chooser, temporary file and commit.
The [Desktop contract](../../desktop/README.md) defines cancellation and shutdown.

For an installed Web package, use `rsi web`. Startup never builds. An explicit
`--assets ABSOLUTE_DIRECTORY` selects a complete verified bundle; its internal
bootstrap identity is independent of the native core. The default installed
package also admits its required Host interfaces before activation.
The local launch document clears its bootstrap fragment immediately, passes its
short-lived ticket to the Rust Worker once, and automatically connects through
the returned cookie identity. It never saves the ticket. Cookie recovery also
supports reloads and additional tabs. Manual device-receipt login remains
available for explicitly composed deployments.

Paired Web output removes the WASM `name` custom section in both profiles.
Unoptimized Rust symbol names alone can exceed 32 MiB and exhaust the immutable
asset owner's 64 MiB aggregate budget. Cargo retains the original intermediate
WASM for symbol investigation; the served artifact retains executable code and
its build family. Resource budgets are unchanged.

Navigation presentation schedules explicit commands and automatic summary reads
through one bounded 32-command lane. It waits for the prior command acknowledgment
before dispatching the next; obsolete summary tickets are discarded before dispatch.
Connection replacement invalidates queued work. Rust remains the ticket and
authorization owner, and the lane never retries a rejected or uncertain mutation.

Image previews capture the rendering connection. Reusing a preview container clears
its previous image and status before checking that connection, so a retired connection
leaves only its unavailable message and starts no read. Detail close captures both its
connection and dialog identity before awaiting, so a late receipt cannot close a
replacement dialog. Global navigation actions capture their owner at invocation.

Document request admission counts each pending waiter once in its selected lane.
Replies (including failures), synchronous transport rejection and connection
retirement release that count; unknown or duplicate replies release no capacity.

A native close also acknowledges settled transport failure during draft flush or
disconnect. Cleanup must complete before that acknowledgement; a failed cleanup
leaves the native deadline responsible for reporting unsuccessful shutdown.

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
Worker termination and renderer cleanup share one teardown result. A rejected
cleanup blocks replacement until the owning document is restarted.
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
