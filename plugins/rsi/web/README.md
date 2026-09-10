# Web document bridge

These assets render the views of the ordinary [Rust Web application](../../../crates/rsi/web/README.md).
The document owns DOM nodes, focus and input delivery; the Dedicated Worker owns
the actual Rust Profile and application. One view crosses the bridge at a time,
acknowledged only after DOM rendering. Input delivery admits eight calls and
coalesces each pane's unsent draft. Switching panes flushes the input handoff;
failed handoffs leave the visible draft intact. Dynamic content enters text
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

Run `node plugins/rsi/web/build.mjs /absolute/output/directory` from the repository.
The build uses the installed `wasm32-unknown-unknown` Rust target and a matching
`wasm-bindgen` executable (`RSI_WASM_BINDGEN` overrides its path). It copies only
the explicit bootstrap, mount bridge and admitted renderer assets consumed by WebAssetsFactory. The directory
must be empty, so a previous generation cannot be served as a mixed bundle.
The default WASM profile is release. Append `--dev` for a debug build with
development assertions; the build receipt names the selected profile.

Open the configured Serve Web origin and paste the JSON receipt from
`rsi --profile devices -- register LABEL`. The receipt is used once and removed
from the form; only the endpoint identity is saved for an explicit reconnect
using the HttpOnly cookie. Sign out drains the Worker application and clears the
cookie. If disconnect or the renderer connection fails, the document terminates
the failed Worker, closes modal details and clears its obsolete view bindings,
then returns to login for explicit reconnection. It does not acknowledge successful sign-out
or cookie removal. A draft handoff failure instead keeps the editable pane open.
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
mounts. Returning to login is not cleanup evidence.

Bound hosts expose only declared action/source membership and requested local
clipboard/focus capabilities. Their authority retires with the slot. Draft fields
are bounded document state, independent of renderer code and preserved only for
the same semantic binding. Modules are operator-admitted trusted same-origin code,
not a JavaScript sandbox; CSP and a closed manifest do not isolate hostile code.
The built-in standard dialog renderer is an ordinary dynamically imported module.

The resident composer retains a local draft through its command acknowledgement
until an ordered application frame echoes it. Moving focus cannot let an older
frame replace newly saved input. Successful submission similarly retains the
local clear until it is echoed; unresolved input keeps its normal retry identity.

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
