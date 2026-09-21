# rsi-language-addon-example

This Cargo workspace uses public source SDK crates, registers its own Service and
Agent factories, and runs provider replacement through `rsi-addon-testkit`.
It has no access to private RSI implementation modules or `.references` resources.
Local path dependencies select the development SDK under test; distributing this
pre-release launcher requires pinning all RSI dependencies to the same published
repository commit, once these APIs have been committed. No source copy is made.

```sh
cargo test --manifest-path fixtures/rsi/lsp-addon/Cargo.toml
cargo run --manifest-path fixtures/rsi/lsp-addon/Cargo.toml -- /absolute/server-config.json /absolute/workspace '{"operation":"definition","path":"src/lib.rs","line":1,"column":1}'
```

The launcher composes public Process, native Sandbox, Files and the language addon,
prints a typed result, and requires clean teardown. Configuration follows the
language plugin's public `Config`; executables are explicitly selected. Tests
exercise lazy activation, replacement and retained-generation retirement without
launching a server or reading user state. Actual peer and pinned rust-analyzer
semantic acceptance belong to the owning language-plugin tests.

After building the launcher, `python3 fixtures/rsi/lsp-addon/verify.py --binary
/absolute/rsi-language-addon-example --analyzer /absolute/rust-analyzer --report
/absolute/new-report` invokes each operation in an independently composed Host,
asserts real semantic locations/text and unchanged source, and requires clean
launcher shutdown. Its explicit JSON configuration disables sysroot discovery.

The Linux example explicitly selects `/usr/bin/bwrap`; an empty Sandbox config
has no restricted backend. Other platforms exercise lazy generation lifecycle
only until a platform-specific Sandbox backend is supplied.

The opt-in final argument `--wait-for-result` repeats empty read queries for at
most 10 seconds while the server indexes; errors are returned immediately. The
ordinary invocation returns its first result, including an empty result.
