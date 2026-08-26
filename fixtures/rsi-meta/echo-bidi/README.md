# rsi-meta-fixture-echo-bidi

`rsi-meta-fixture-echo-bidi` is the native ABI v2 provider intended for Loader
integration. It exports only ABI v2 through the safe Rust SDK. Each preparation
declares the actual `upstream` injection and a conservative retained-byte charge
for its prepared state. Activation opens the Runtime-installed
effect transaction, defers cleanup, dynamically provides the `echo` port, and
requests commit; after the callback succeeds, the SDK asks the host adapter to
accept the native subprotocol state. The outer Runtime still owns final root
commit or rollback. One `SERVE_PORT` callback receives an exact
provider-oriented channel:
it receives one Message and request EOF, then uses that provider capability as
the explicit callback scope when it opens the injected upstream caller channel.
It half-closes upstream requests, requires one response and clean terminal,
prefixes the bytes, and preserves returned transferable capabilities. Bounded
test-only delay and gate fields let integration tests prove callback-watchdog
and offloaded-destruction behavior.

This plugin-owned slice proves the safe table and builds a real host-platform
dynamic library. It does not prove that [`rsi-meta-loader`](../../../crates/rsi-meta/loader/)
has adopted ABI v2; Loader host tables, callback/effect bridging, admission, and
mapping/unload remain their own integration gate. This fixture is test evidence,
not a separately supported plugin distribution. Run its standalone checks:

```sh
cargo metadata --locked --offline --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --format-version 1 --no-deps
cargo fmt --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --check
cargo clippy --locked --offline --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --all-targets -- -D warnings
cargo test --locked --offline --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --all-targets
cargo build --release --locked --offline --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml
```

The unit test calls the exported entry and validates the returned v2 table
header. These commands prove only the host platform on which they run. On
Linux, the conformance command also inspects the release artifact's ELF dynamic
symbol table: `rsi_meta_plugin_entry_v2` must be its only
`rsi_meta_plugin_entry_*` export.
