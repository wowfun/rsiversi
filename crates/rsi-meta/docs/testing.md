# rsi-meta testing

Behavior tests use only public `Runtime`, `Context`, `PluginFactory`, Fiber,
service, and event interfaces. They cover dependency convergence and cycles,
exact contracts, private isolation, direct-edge intercepts, call and generation
fencing, bounded frames, event modes, setup rollback, reconfiguration,
dependent retirement, old-child retirement during parent reconfiguration,
multi-child reverse application order, child-before-parent cleanup, reverse
effects, and joinable disposal. Invariant
tests additionally exercise apply and shutdown cancellation, staged-listener
rollback, activation/event/cleanup panic containment, activation and shutdown
deadlines, admission linearization, complete call and dispatch deadlines,
queued cancellation, serialized reconfiguration, descriptor observation,
trace ancestry, global listener limits and authority, exact-once claiming
including consumption after a timed-out attempt,
concurrent safe-Rust callbacks, terminal-error delivery under backpressure,
bounded handler-produced event values, bounded input and normalized plugin
configuration, consistent validation-panic classification,
drain-before-cleanup, and ownership release. Event evidence includes prepend ordering and scoped dispatch with both
isolated and global listeners, plus pointer-identity evidence that immutable
inputs are shared across listeners. Limit evidence exercises every nonzero runtime
limit plus reconciliation, live service-call, configuration, service, and per-Fiber effect quotas. Caller request frames and provider response frames are tested at the same bound. A public release-mode probe separately guards the reachable-only cycle-diagnostic cost against an unrelated-pending-Fiber regression and persistent Context scope construction against full-map cloning. Reconfiguration and public-
disposal cancellation plus concurrent root shutdown prove that admitted
transactions remain Runtime-owned and one shutdown deadline is not multiplied
by root count.

ABI tests exercise table layout, null-zero inputs, allocator-matched ownership,
panic containment, and minor-version direction. Loader tests build and map a real host-platform dynamic
library, cross the C ABI, invoke a required host service from native code on
both multi-thread and current-thread Tokio runtimes, and return through a core
service call. They also prove whole-config preflight creates no child Fiber on
failure, apply-loop failure rolls back every earlier child, normalizers run
once, FIFOs and oversized
artifacts are rejected before mapping, timeout watchdogs terminalize even when
core has already dropped an adapter future, timed-out factories remain
poisoned, create and call callbacks do not consume Tokio's shared blocking
pool, destruction stays off the executor, failed creates release a non-null
partial instance, duplicate Loader IDs remain atomic under concurrent service
calls, cancelled commands release their ID reservations, distinct artifacts
preflight with bounded concurrency, IDs stay reserved through unload cleanup,
cache symlinks and collisions fail closed, and in-place cache mutation cannot
change a Unix staged artifact. A live mapped digest is reused without touching
an unrelated durable-cache mutation, while the same collision still fails a
later cold load. Malformed returned ABI buffers are rejected
without skipping their allocator-matched release callback. Timed-out artifact
entry workers fence same-digest re-entry until they return, instance destruction
waits for a timed-out call to exit, and explicit entry failure status remains
distinct from ABI-table incompatibility. A virtual-time worker test proves a
completed callback releases its independently owned watchdog resources without
waiting for the deadline.

For any foundation change, run:

```sh
cargo fmt --all --check
cargo clippy --locked -p rsi-meta --all-targets -- -D warnings
cargo clippy --locked -p rsi-meta-plugin --all-targets -- -D warnings
cargo clippy --locked -p rsi-meta-loader --all-targets -- -D warnings
cargo test --locked -p rsi-meta --all-targets
cargo test --locked -p rsi-meta-plugin --all-targets
cargo test --locked -p rsi-meta-loader --all-targets
cargo xtask rsi-meta code-health
```

`cargo xtask rsi-meta conformance` runs this package-level evidence. Actual native execution claims apply only to the host platform exercised; Windows and macOS native runs are not inferred from Linux evidence.
