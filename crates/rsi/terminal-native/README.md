# rsi-terminal-native

Independently compiled native terminal presentation plugin. It exports the
`rsi.terminal.render` Portable service and uses the same pure presentation library
as the linked renderer. It owns no terminal modes, signals, credentials, Session
controller, backend, native loader or standard product composition.

Configuration is null. The enclosing presentation Profile pairs this plugin with
`rsi.terminal.portable`. Scene metadata is a validated UI model followed by the
exact declared source length in raw chunks of at most 64 KiB. Requests must end
cleanly before rendering. Replies are bounded binary full cell frames with their
exact attachment, presentation, revision and source map. Native callbacks never
write ANSI to the terminal.

Build this standalone workspace with:

```bash
cargo build --locked --manifest-path crates/rsi/terminal-native/Cargo.toml
```

The `revision-b` feature supplies a visible test marker to prove actual code
replacement through the native SDK. It is not enabled for developer builds.

This standalone workspace owns its lockfile. Shared dependencies need not have
identical Rust versions: the host/plugin boundary is the versioned native byte
ABI, with no Rust objects crossing it. When changing this manifest or updating
its dependencies, explicitly review `cargo update --manifest-path
crates/rsi/terminal-native/Cargo.toml` and its lockfile diff, run Clippy with
`--all-targets` with default features and with `--all-features`, check warning-free
`cargo doc --no-deps --all-features`, and run the terminal application's
`native_presentation` integration test. CI lints both presentation revisions;
the real-loader test verifies compatibility with the current host.
