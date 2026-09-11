# rsi-desktop-admission

This isolated fixture validates Tauri 2.11.5 using the same serde_json number
and object-order features as the root workspace. It executes real IPC and DOM
rendering in Linux WebKitGTK; it is not a Session or live-provider test.

Run `cargo run --locked --manifest-path fixtures/rsi/desktop-admission/Cargo.toml`
inside a graphical session or `xvfb-run -a`. The page reports its actual command,
number round-trip, raw protocol and 128-block checks to stdout and requests an
orderly exit. A failed check returns a nonzero process status. No credentials
or user state are used. Set `RSI_ADMISSION_REPORT` to an absolute JSON output
path to retain evidence. The process uses a temporary WebView data directory.

For reproducible native typing and screenshots, run `verify.py --binary PATH
--driver PATH --report NEW_DIRECTORY` under `xvfb-run -a dbus-run-session --`.
It uses WebKitWebDriver directly with Tauri's WebView automation context and a
private data directory, then checks that main-thread exit follows the owned async
drain. The small drain fixture is platform-loop evidence; actual Host cleanup
and borrowed/embedded ownership have separate product tests.
