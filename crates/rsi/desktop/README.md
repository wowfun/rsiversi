# rsi-desktop

The Linux desktop adapter composes ordinary Application-only GUI, navigation and
setup plugins with the standard Service connection. It uses the shared Web
document and `rsi-gui`; Tauri owns only the main-thread event loop, a private
persistent WebView data directory and the native document transport. The headless
`rsi` executable has no dependency on this package or GTK/WebKit.

Start with `rsi-desktop --assets /absolute/bundle [--host-profile standard]`.
The companion `rsi` beside this executable is the canonical coding helper.
Distribution builds must compile both binaries from the same frozen build-family
manifest; see the Service Host compatibility contract. This is compatibility
identity, not authentication. The ordinary connection attaches to an exact daemon
or owns an embedded Service. Closing the GUI drains the Application and owned
Service; a borrowed daemon remains alive.

The `rsi` custom protocol serves the existing closed immutable asset owner and a
native mailbox, never arbitrary paths. Only the main window at `rsi://localhost`
can call its private routes. The main document has one Tauri window-close
capability, which enters the same document drain path; no filesystem capability
is granted. Startup composition, protocol admission and Runtime/event-loop
lifetime each have one internal owner. A single view request and one pending frame are
admitted. The shared 32-MiB frame bound remains authoritative. ACK names its exact
frame; a 30-second missing ACK requests Application teardown. Ordinary requests
have eight non-queued slots; ACK and disconnect have reserved admission. Binary
image/source responses travel as raw bytes, without JSON arrays or base64.
Tauri's response handoff requires owned bytes, so it copies the shared retained
buffer. Wry/WebKit also buffer platform requests before this adapter sees them.
The owner limits bound admitted logical work and pending frames; they are not
RSS limits or a claim of zero-copy platform delivery.
Renderer candidate and accepted graph leases remain distinct until an exact
document ACK reports successful mount and awaited old-renderer disposal.

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
