# rsi-ai testing

Default evidence is deterministic, keyless, and isolated from real user state.
Protocol tests cover closed Language/Image request schemas, constructor and
aggregate bounds, recursive JSON limits, stream grammars, prepared snapshots,
and dispatch error facts. Router tests prove exact deployment routing,
generation withdrawal, provider-I/O-free Prepare, consuming Start, and
independent Language/Image availability.

Provider tests use local HTTP servers and assert paths, headers, bodies,
provider-specific setting rejection, stream translation, reasoning and Tool
replay, usage, media resolution, request-level atomic media admission, multipart
body projection, last-waiter admission cancellation, and error classification.
Transport tests fragment SSE, apply provider-selected finite frame bounds,
exercise admission growth and waiter cancellation, prove overlapping large
claims can make progress without an unsafe allocation state, prove an
ungrantable growth waiter cannot block a separately safe new-frame admission, exercise
deadlines, and prove cancellation after headers. Deferred Language tests prove one-request operations, closed
checkpoints, monotonic cursors, atomic event/checkpoint batches, and interrupted
stream handling. Protocol and provider suites reject semantically invalid
checkpoints during deserialization, while router restore tests reject frozen
route-fact drift including retry policy. OpenAI restore tests reject legacy parser-state versions and
non-digest open-block keys while accepting checkpoints emitted by the current
format.

Use these Linux checks for the complete family:

```text
cargo test --locked -p 'rsi-ai*' --all-targets --all-features
cargo clippy --locked -p 'rsi-ai*' --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p 'rsi-ai*' --no-deps
cargo xtask verify-docs
```

Ignored live-provider tests are opt-in smoke tests, not release-gate evidence.
They require explicit environment credentials and prove only the provider and
platform actually exercised.
