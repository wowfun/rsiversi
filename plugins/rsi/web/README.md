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
the six explicit production assets consumed by WebAssetsFactory. The directory
must be empty, so a previous generation cannot be served as a mixed bundle.
The default WASM profile is release. Append `--dev` for a debug build with
development assertions; the build receipt names the selected profile.

Open the configured Serve Web origin and paste the JSON receipt from
`rsi --profile devices -- register LABEL`. The receipt is used once and removed
from the form; only the endpoint identity is saved for an explicit reconnect
using the HttpOnly cookie. Sign out drains the Worker application and clears the
cookie. If disconnect fails, the document terminates the failed Worker and returns
to login for explicit reconnection; it does not acknowledge successful sign-out
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
