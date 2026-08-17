# rsi-meta testing and conformance

The conformance suite observes behavior only through `CompositionHost`, the C ABI, wire envelopes, and process-owned files. Private registry, store, routing, and loader types are not compatibility surfaces.

## Release evidence

| Suite | Owned evidence |
|---|---|
| Graph and lifecycle | Scope resolution, inactive instances, atomic cutover, generation leases, HMR, and retirement from the [composition runtime](subsystems/composition-runtime.md) |
| ABI and package | Maintained header layout, safe SDK trampolines, package validation, and real `cdylib` loading |
| State and recovery | Workspace leases, pair integrity, domain-operation idempotency, strict single-version store rejection, bounded retention/quotas, apply/install recovery, and failpoints |
| Daemon and stream | Local adapters, replay cursor, reconnect reasons, and credit-bounded streams from the [protocol contract](subsystems/protocols.md) |
| Security | Local-only admission, credentials, ownership checks, redaction, bounds, and fail-closed behavior from [security.md](security.md) |

Concurrency tests use explicit gates, channels, or a fake tick source; they do not infer ordering from wall-clock sleeps.

## Choosing verification

| Changed surface | Required verification |
|---|---|
| Private code in one root package | Package formatting, Clippy, and `cargo test --locked -p <package> --all-targets` |
| Graph, routing, lifecycle, HMR, CAS, or public host behavior | Focused `rsi-meta` tests plus `cargo xtask rsi-meta conformance` for public behavior |
| Persistence, command deduplication, or recovery | Focused tests plus `cargo test --locked -p rsi-meta --all-targets --features test-failpoints` |
| Loader staging or interrupted filesystem work | Loader tests plus `cargo test --locked -p rsi-meta-loader --test staging_crash` |
| C ABI, SDK, plugin frames, manifests, or lifecycle lanes | Plugin tests plus `cargo xtask rsi-meta conformance` |
| CLI parsing or transport internals | CLI tests; add `cargo test --locked -p rsi-meta-cli --all-targets --features test-failpoints` for durable-before-ack or restart behavior |
| Schema, wire serialization, or protocol docs | Owning DTO/schema tests plus the conformance schema-contract tests |
| One standalone plugin or fixture | Its manifest-scoped format, Clippy, and tests; use full conformance for shared contracts or package-list changes |
| Toolchain, CI, dependencies, product-wide validation, or a release claim | The complete development checks, dependency audit, conformance xtask, and release demonstration |

The syntax-aware `cargo xtask rsi-meta code-health` gate covers all production Rust files in the core composition/routing/runtime/persistence areas and the loader. It excludes `#[cfg(test)]` items, blank lines, and comment-only lines, rejects files above 1,200 lines, and rejects any increase in an area's checked-in maximum. `--write` may only lower an existing baseline.

Root `--workspace` does not cover standalone workspaces below `plugins/rsi-meta/` or `fixtures/rsi-meta/`.

## Release demonstration

[`fixtures/rsi-meta/release-demo`](../../../fixtures/rsi-meta/release-demo/README.md) must demonstrate, from empty state: lock resolution, initial offline install, daemon startup, a gap-free snapshot/cursor pair, activation by hot apply, a bidirectional bounded stream, provider replacement with an old lease pinned, failure after durable commit but before acknowledgement, retry recovery, then the process-fixed `apply -> exit 75 -> daemon stop -> install -> fresh daemon serve` sequence.

The full local release entrypoint is `cargo xtask rsi-meta conformance`; it runs standalone package checks, real-library conformance, and the thirteen-step demonstration. Use `cargo xtask rsi-meta release-demo` for only the assembled scenario.

Only Linux x86_64 and macOS arm64 CI together support the full two-platform conformance claim. A local run reports only its actual host platform and commands.
