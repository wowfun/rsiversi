# rsi-meta-fixture-echo-bidi

`rsi-meta-fixture-echo-bidi` is a real native provider used by loader integration tests. It implements the v1 fixed-layout ABI, requires `upstream`, provides `echo`, forwards one bounded request through the host service callback, and prefixes the response from its validated JSON configuration. Bounded test-only delay fields let integration tests prove callback-watchdog and offloaded-destruction behavior.

[`rsi-meta-loader` tests](../../../crates/rsi-meta/loader/tests/native_loader.rs) compile and map the resulting host-platform dynamic library. This fixture is test evidence, not a separately supported plugin distribution.

The loader integration suite is the authoritative execution gate because it
crosses the real host dynamic-library boundary. When changing the fixture
itself, also run its manifest-scoped checks:

```sh
cargo fmt --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --check
cargo test --locked --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml
cargo clippy --locked --manifest-path fixtures/rsi-meta/echo-bidi/Cargo.toml --all-targets -- -D warnings
```

These commands prove only the host platform on which they run.
