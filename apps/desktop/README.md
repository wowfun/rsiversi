# rsi-desktop

Desktop is an optional executable package. `rsi desktop` starts or attaches to
the selected product Host, pins the installed executable and launches it in the
caller’s display environment. Desktop does not compose a Service, open an Agent
Store, run a native GUI model, or locate a companion executable.

With an explicit frontend source recipe, the invoking Application owns Vite and
its backend connection. It passes the admitted launch URL through one bounded
private stdin frame within thirty seconds, then retains that pipe as a lifetime lease. The managed
Desktop package validates this handoff before creating the main window and
uses the same exact-Origin native bridge checks. Ordinary installed Desktop
continues to own its own Host listener connection.

The executable receives resolved `--config-home`, `--state-home` and
`--cache-home` directories from its launcher. `--assets` optionally selects an
explicit local Web bundle; otherwise the Host resolves the installed `web`
export. Direct invocation requires all three absolute homes and an existing
Host. The Host validates its native peer and Web control descriptor, then owns a
loopback listener for the lifetime of this Desktop connection. Closing the
window releases that listener and leaves the shared Host running.
An installed executable also retains its own package lease. Its launcher's
control input closes when that Application attachment retires or disappears;
Desktop then uses the same draft-draining close path as a window close.

The window loads the ordinary Web document and Worker using a one-use launch
ticket. Its private WebView data lives beneath the selected state home.
The Host's [Desktop listener contract](../serve/README.md#resident-product-listeners)
preserves its Origin across restarts so that saved drafts remain accessible.
Windows requires the Microsoft Edge WebView2 Runtime; Linux requires WebKitGTK.
The headless core does not depend on either runtime.

The [Desktop bridge](../../crates/rsi/desktop-bridge/README.md) is independent of
the Worker transport. Tauri grants only its single product command, to the main
window and the loopback Origin chosen for this attachment. Navigation
admits that root and the two same-Origin sandbox preview bootstrap paths;
WebKit applies this navigation gate to child frames too. Tauri capabilities apply to the Origin, not a document path. Preview documents
have an opaque Origin enforced by their HTTP sandbox CSP, which denies native IPC
and parent-document access. That mandatory CSP and the opaque-frame channel own preview
isolation. Each
request also checks the main window, current URL and generated descriptor.
At most eight native requests run concurrently; excess requests fail with a busy
diagnostic before admission, including export chunks.
Dialog and clipboard plugins grant no document-accessible plugin commands.

Native export consumes the Worker’s existing bounded export events. Rust owns
the save dialog and destination; JavaScript never supplies an output path.
The shared [atomic sink](../../crates/rsi/session-export/README.md) verifies
offsets, byte count, SHA-256 and stream completion before replacement. Only one
export is active; cancellation discards its temporary file. An admitted commit
is awaited even if its JavaScript waiter disappears. The platform chooser has no
portable dismissal operation, so ordinary export cancellation retains its slot
until the callback. Application shutdown releases the callback waiter before
destroying its owning window; a late selection is ignored and cannot start a write.

A close requested before the document registers its listener is retained and
delivered when its authenticated bridge Hello arrives, under the original
watchdog deadline.
Window-manager close, Host listener loss, and the document’s Close action flush drafts, disconnect
the Worker, then acknowledge native close. Failed draft persistence cancels the
close attempt and leaves the window available for recovery. A failed document can acknowledge once cleanup settles; an unresponsive
document reports a timeout and drains native work before exit.
Clipboard authority is write-only, with a 64 KiB UTF-8 bound.

The native build embeds `icons/icon.ico`; `icons/icon.png` contains the same
artwork. The [Web logo](../web/public/logo.svg) is the artwork source. To
regenerate the icons, run `python icons/generate.py` with Pillow and resvg 0.45.1,
optionally specifying `--resvg /absolute/path/to/resvg`. Icon generation does
not run during normal builds.

Use the package’s unit tests for origin, option and cleanup boundaries, and
verify a built Desktop against the same admitted Web bundle used by browser
checks. Native macOS runtime verification requires a native macOS environment.
