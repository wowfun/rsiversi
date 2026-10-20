# rsi-desktop

The native build embeds the committed multi-resolution application icon from
`icons/icon.ico`; `icons/icon.png` contains the same artwork at 256 pixels.
These assets permit Windows resource compilation. Windows 11 24H2+ x64 uses
WebView2 and embedded Host ownership; Linux uses WebKitGTK and existing Host
attachment. A relocatable distribution still requires the platform system runtimes,
including VC++ Runtime and WebView2 on Windows; it is not an installer.
The [Web logo](../web/public/logo.svg) is the sole artwork source, following the
[shared graphical brand contract](../web/README.md). Run `python icons/generate.py`
with Pillow and resvg 0.45.1 installed to regenerate both raster assets; use
`--resvg /absolute/path/to/resvg` when the pinned renderer is not on PATH.
The generator renders curves and gradients on a transparent canvas, preserves
the SVG viewBox and writes ICO sizes 16, 24, 32, 48, 64, 128 and 256.
It never runs during a normal build.

Session export opens a Rust-owned native save dialog, then consumes the shared
verified export stream into the [atomic file sink](../../crates/rsi/session-export/README.md).
The document supplies only a filename hint and selected pane; it gains no general
filesystem or Tauri IPC authority. Cancel and close discard incomplete files.

The Linux and Windows desktop adapter composes ordinary Application-only GUI, navigation and
setup and Plugins workbench plugins with the standard Service connection. It uses the shared Web
document and `rsi-gui`; Tauri owns only the main-thread event loop, a private
persistent WebView data directory and the native document transport. The headless
`rsi` executable has no dependency on this package or GTK/WebKit.
The standard Application catalog owns setup and Plugins registration shared with TUI. The
desktop addon registers only its GUI, navigation and native transport factories;
its Profile selects both shared factories without registering them again.
Directory selection explicitly activates the shared directory API client after
the native connection publishes its transport, before GUI activation. An embedded
Service and a borrowed daemon use the same client contract as the Worker.

Start with `rsi-desktop [--assets /absolute/assets] [--host-profile standard]`.
Assets default to the directory beside the executable, independently of the
working directory. A missing assets directory is rejected before creating state
or starting a Service; the asset owner still validates the complete bundle.
Generation lease admission precedes asset filesystem checks and WebView2 probing.
Duplicate options are rejected. Windows checks WebView2
availability after help and before creating state or starting a Service.
On Windows, the state root and WebView directory are created through the native
no-follow private-directory API and their protected ACLs are verified before
Service startup. The verified state root handle remains the authority for creating
or opening its WebView child; child setup does not traverse the absolute state
path again. Each requires the canonical protected DACL: exactly SYSTEM,
Administrators and the current token user, each with full control and object/container
inheritance. Existing directories with any different DACL, including a narrower
user-only ACL, are rejected with the failing path and recovery guidance. Choose a
fresh state directory using `XDG_STATE_HOME` (the application appends `rsi`), or have the owner review and restore
that exact ACL before retrying; the desktop entry never silently repairs existing state.
The companion `rsi` (`rsi.exe` on Windows) beside this executable is the canonical
coding helper. Bash selection uses the [standard product launcher](../../crates/rsi/core/README.md).
Generation pin failures report a normal startup error with exit code 2 before
opening a window; they never enter the panic handler.
Distribution builds must compile both binaries from the same frozen build-family
manifest; see the Service Host compatibility contract. This is compatibility
identity, not authentication. The ordinary connection attaches to an exact daemon
or owns an embedded Service. Closing the GUI drains the Application and owned
Service; a borrowed daemon remains alive.

The `rsi` custom protocol serves the existing closed immutable asset owner and a
native mailbox, never arbitrary paths. Navigation admits the application root and
`/preview-local.html` or `/preview-online.html` at the platform document origin:
`http://rsi.localhost` on Windows WebView2 and `rsi://localhost` elsewhere. Wry
normalizes a Windows custom-protocol callback URI back to `rsi://localhost`, while
the document's Origin header and CSP use its actual HTTP origin. These immutable
preview responses use opaque, script-enabled sandbox policies, respectively with
external resources disabled or explicit HTTPS access; neither exposes native routes.
Errors on those exact preview paths retain their sandbox response policy and are
JSON with nosniff, even when the asset owner is absent or lookup fails.
Only the main window with a POST method can call private routes, including frame
polling. Exactly one Origin header must name the platform document origin.
Linux WebKitGTK omits Origin on same-origin custom-scheme POSTs; that case instead
requires exactly one Referer naming the application root `rsi://localhost/`.
Missing both headers, a preview-page Referer in that case, or a malformed,
foreign or duplicate Origin is rejected. Initial immutable asset GETs may omit
Origin. The shared
[Web document mode](../web/README.md) selects the corresponding native transport.
The document is granted no
Tauri IPC command permissions. Dialog and clipboard plugins supply Rust-owned
native operations; registering them does not grant document command access.
Window-manager close events and the
application's existing Close action enter the same document drain path; JavaScript
cannot invoke the Tauri window-close command. Startup composition, protocol admission and Runtime/event-loop
lifetime each have one internal owner. A single view request and one pending frame are
admitted. The shared 32-MiB frame bound remains authoritative. ACK names its exact
frame; a 30-second missing ACK requests Application teardown. Session operations
remain independent of document refresh. A new document connection retires the
previous document's frame wait, renderer offer and ACK timer, then waits for its
actual frame request to leave the lane before opening a new snapshot stream.
This does not cancel Session work or replay submitted commands. A forced WebView
reload destroys its old renderer context; normal close still awaits disposal.
Ordinary requests
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

Native export preparation is cancellable. An open save dialog retains the export
slot until the user selects a path or dismisses it; the dialog API cannot dismiss
it programmatically. Cancellation discards any later selection. Application cleanup
also waits for this callback. After the file sink admits persistence, cancellation
keeps the slot occupied until actual success or filesystem failure. Bridge tasks
retain the worker across request-waiter loss and application cleanup joins that work.

Native file export first reserves an opaque operation token through `export_open`;
`export_save` and `export_cancel` must carry that exact token. Unclaimed reservations
expire after thirty seconds. Claim wins when claim and expiry are simultaneously
ready; expiry rechecks claim and reservation identity under the owner lock. Cancellation before save admission removes only that
reservation; an old token cannot affect a later export. Cancellation has a separate
bounded control lane in the document and native bridge, and its caller awaits
admission, retrying only explicitly rejected Busy responses.

Clipboard writes use the native clipboard manager behind the shared document
capability, with a 64 KiB UTF-8 limit at both document and native boundaries.
Only a successful native receipt displays success; `rsi://` does not rely on
Navigator clipboard support. No clipboard read capability is exposed.

## Development builds

For Linux, install the GTK 3 and WebKitGTK 4.1 development libraries
(`libgtk-3-dev` and `libwebkit2gtk-4.1-dev` on Ubuntu), and the Web build tools
described in [source installation](../../docs/user/source-install.md). From the
repository root in a graphical session, publish a debug distribution to a new
absolute directory and launch its Desktop executable:

```bash
cargo run --locked -p rsi-app-tools -- dist desktop /absolute/new/output --debug
/absolute/new/output/bundle/rsi-desktop
```

On native Windows, use `scripts/rsiversi.ps1 setup desktop`,
`scripts/rsiversi.ps1 build desktop --debug`, then `scripts/rsiversi.ps1 desktop`.
MSVC and a Windows SDK are build prerequisites; WebView2 is checked at launch.
The [source installation tutorial](../../docs/user/source-install.md) owns the
PowerShell invocation, system prerequisites and managed build targets.
