# rsi-desktop

Session export opens a Rust-owned native save dialog, then consumes the shared
verified export stream into the [atomic file sink](../session-export/README.md).
The document supplies only a filename hint and selected pane; it gains no general
filesystem or Tauri IPC authority. Cancel and close discard incomplete files.

The Linux desktop adapter composes ordinary Application-only GUI, navigation and
setup and Plugins workbench plugins with the standard Service connection. It uses the shared Web
document and `rsi-gui`; Tauri owns only the main-thread event loop, a private
persistent WebView data directory and the native document transport. The headless
`rsi` executable has no dependency on this package or GTK/WebKit.
The standard Application catalog owns setup and Plugins registration shared with TUI. The
desktop addon registers only its GUI, navigation and native transport factories;
its Profile selects both shared factories without registering them again.

Start with `rsi-desktop --assets /absolute/bundle [--host-profile standard]`.
The companion `rsi` beside this executable is the canonical coding helper.
Distribution builds must compile both binaries from the same frozen build-family
manifest; see the Service Host compatibility contract. This is compatibility
identity, not authentication. The ordinary connection attaches to an exact daemon
or owns an embedded Service. Closing the GUI drains the Application and owned
Service; a borrowed daemon remains alive.

The `rsi` custom protocol serves the existing closed immutable asset owner and a
native mailbox, never arbitrary paths. Navigation admits the application root and
`/preview-local.html` or `/preview-online.html` at `rsi://localhost`. These immutable
preview responses use opaque, script-enabled sandbox policies, respectively with
external resources disabled or explicit HTTPS access; neither exposes native routes.
Errors on those exact preview paths retain their sandbox response policy and are
JSON with nosniff, even when the asset owner is absent or lookup fails.
Only the main window at `rsi://localhost`
can call its private routes. The document has no Tauri IPC capabilities. Window-manager close events and the
application's existing Close action enter the same document drain path; JavaScript
cannot invoke the Tauri window-close command. Startup composition, protocol admission and Runtime/event-loop
lifetime each have one internal owner. A single view request and one pending frame are
admitted. The shared 32-MiB frame bound remains authoritative. ACK names its exact
frame; a 30-second missing ACK requests Application teardown. Ordinary requests
have eight non-queued slots; terminal reads have 32 and writes have eight.
Connect and disconnect share one lifecycle slot. ACK and document-close signals
use synchronous control paths. Transport classification controls budgets only;
GUI owns terminal validation. The bounded classification parse runs before taking
the Owner lock; GUI separately parses and validates the body during execution.
The Owner lock still fences task registration against close, without covering
JSON classification. HTTP 409 errors carry `code`, `message`,
`notAdmitted` and `retryable`. Only a `busy` rejection before dispatch is
retryable. `closed` and `invalid` are permanent rejections; `failed` means the
operation has been polled and cannot authorize replay. A stop observed before
the first operation poll returns `closed`; after that poll it returns `failed`,
even if the operation has not yet produced a visible effect. Unknown or lost replies
never establish non-admission.
Frozen assets use a separate synchronous lookup/copy path with one copy at a
time on the protocol callback thread; on Linux this is the WebKit main thread.
This performs no disk I/O, but its bounded memcpy can delay that event loop.
RPC saturation cannot reject an asset import. Closing fences new copies
and joins an existing copy; response handoff holds no asset or owner lock.
Request admission ends when the operation completes, before its response is handed
to WebKit. A response consumer can immediately issue its next request without
competing with the completed operation's permit.
Binary image/source responses travel as raw bytes, without JSON arrays or base64.
Tauri's response handoff requires owned bytes, so it copies the shared retained
buffer. Wry/WebKit also buffer platform requests before this adapter sees them.
The owner limits bound admitted logical work and pending frames; they are not
RSS limits or a claim of zero-copy platform delivery.
Renderer candidate and accepted graph leases remain distinct until an exact
document ACK reports successful mount and awaited old-renderer disposal.
Serialized renderer offers are retained by revision across frames and resync.
Native frames enter the document as decoded objects without a stringify/parse
round trip.

Install Tokio before constructing Tauri, then run Tauri on the main thread.
Exit requests prevent platform exit while the retained ApplicationLifetime future
joins actual Runtime cleanup. Cleanup failure is an unsuccessful exit. Window
close and application quit use that same stop path.
Close first asks the document to flush drafts and await renderer disposal. A
failed draft flush cancels that close attempt and its deadline, preserving the
window for explicit draft recovery. A later close uses a fresh attempt. A
missing document drain or frame ACK after 30 seconds marks an unsuccessful exit
and requests Runtime teardown; it never reports successful document cleanup.
WebView data lives below the
operator-selected RSI state directory with owner-only Unix permissions; it is
not a temporary directory and is never shared with the browser's Device store.

Validation uses actual Linux WebKitGTK/WebDriver, isolated Host paths and the
shared product fixtures. Native Windows/macOS behavior is not established by
Linux builds or browser tests.
