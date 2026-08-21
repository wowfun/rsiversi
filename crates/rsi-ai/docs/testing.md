# rsi-ai testing

Default evidence is deterministic and keyless. Protocol tests cover closed
request schemas, canonical JSON, semantic bounds, strict language/media stream
grammars, binary chunk corruption, and Realtime state transitions. Core tests
exercise exact routing, prepare/start separation, cancellation, all five
capability handles, and scripted interactive Realtime behavior.

Concrete provider tests run local HTTP or WebSocket servers and assert exact
paths, headers, request bodies, provider-specific unsupported-setting checks,
stream translation, error classification, reasoning/tool replay, usage, and
media handling. Transport tests fragment SSE, bound bodies, exercise finite
request deadlines, and prove cancellation after response headers. Realtime
tests prove bounded handshakes and cancellation during socket I/O. Auth tests use
in-memory stores and captured environments, never a real keyring or user state.
Each production plugin factory is also built without network I/O and checked
against the exact capability list in its `plugin.toml`; unknown config fields
must fail closed. `cargo xtask rsi-ai conformance` additionally compiles every
plugin config schema, stages and maps each release cdylib through the real
loader, then drives prepare, commit, retire, and shutdown through its exported
ABI entry point with a non-secret placeholder credential.
OpenAI deferred tests prove provider-I/O-free Prepare, background submission,
single-request polling/cancellation, closed checkpoint JSON, monotonic cursor
validation, clean interrupted-stream EOF, and `starting_after` recovery through
a local HTTP server.

`rsi-ai-meta` tests control and binary wire contracts. The `rsi-agent`
conformance gate builds native scripted plugins and runs a real composition to
cover all five services, generation pinning, credit, durable barriers, CAS
bytes, restart, and replay without live credentials.

Use these Linux checks for the changed surface:

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --manifest-path plugins/rsi-ai/Cargo.toml --workspace
cargo clippy --manifest-path plugins/rsi-ai/Cargo.toml --workspace --all-targets -- -D warnings
cargo xtask rsi-agent conformance
cargo xtask verify-docs
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --lib --no-deps
cargo test --locked --workspace --doc
```

Live-provider smoke tests require an explicit opt-in and are not release-gate
evidence. Windows is unsupported by `rsi-agent`; macOS native loading and
keyring behavior require a macOS runner.

The ignored live tests are `deepseek_v4_flash_streams_a_real_completion` and
`xiaomi_token_plan_synthesizes_then_transcribes_real_audio`. They read secrets
only from the process environment. Xiaomi defaults to the official China Token
Plan origin; set `XIAOMI_TOKEN_PLAN_BASE_URL` to the account's displayed
OpenAI-compatible base URL when it belongs to another cluster. The Xiaomi smoke
requires a recognizable TTS-to-ASR round trip; because both outputs are
provider-generated, it permits one complete semantic retry, while transport,
authentication, protocol, and timeout failures remain immediately terminal.
