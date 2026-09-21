# Language addon protocol peers

`test_peer.py` is a deterministic stdio Content-Length peer used by `rsi-lsp`
behavior tests. It records received document text and positions to a private
fixture log and supports explicit malformed, hanging and exiting modes. It never
reads developer credentials or real projects. Unit scenarios use real Process
pipes with an explicitly unconfined test Sandbox; native rust-analyzer acceptance
is separate and exercises actual restricted Linux confinement.

Real opt-in acceptance:

```sh
RSI_RUST_ANALYZER="$(rustup which --toolchain 1.97.0 rust-analyzer)" cargo test -p rsi-lsp pinned_rust_analyzer_four_queries_under_native_read_only_sandbox -- --ignored --nocapture
```

This asserts `rust-analyzer 1.97.0 (2d8144b 2026-07-07)`, uses actual Bubblewrap,
creates a private dependency-free Rust project and verifies all four semantic
results, including a cursor after an emoji. Source bytes remain unchanged.

`prepare.py` checks the exact native rust-analyzer version, creates an isolated
Rust project, and appends explicit language and UI Host leaves. Browser and
desktop scenarios exercise actual authenticated standard UI actions, UTF-16
locations and opening the current file, separately from protocol peer tests.
Set `RSI_RUST_ANALYZER` to the pinned executable; no installer is invoked.

With the same explicit server, `RSI_WEB_ASSETS=/absolute/assets node
fixtures/rsi/lsp/browser.mjs /absolute/report` verifies Chromium and Firefox.
`cargo test -p rsi --test service_host_cli language_tui_opens_real_definition_and_closes_service_observation -- --ignored`
exercises the actual PTY; `RSI_TUI_PTY_REPORT` retains cell frames. The desktop
[product fixture](../desktop-product/README.md) accepts `--language /absolute/rust-analyzer`.
All three open the actual definition after an emoji-containing query and preserve
the source; the browser and TUI perform no provider requests.

The browser also pages a real reference result after replacing its source with
an oversized file: the cached second page must remain available, while explicit
Repeat query must fail source admission. Restoring the private source makes
Repeat query succeed again. Both engines retain screenshots and the restored
source, with no inference requests.
