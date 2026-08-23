# rsi-meta-fixture-cycle-probe

This release-mode probe exercises the public `Runtime`, `Context::apply`, and
pending-Fiber snapshot interfaces. It doubles a large population of unrelated
Fibers that all require one permanently missing service, then builds an equally
scaled public `Context::isolate` chain. It fails when either observed growth
becomes superlinear or exceeds a broad absolute deadline. This guards the
reachable-only declaration-index and persistent copy-on-write Context
contracts without depending on private runtime state or a checked-in
machine-specific timing baseline.

Run the probe from the repository root:

```sh
cargo run --release --locked --manifest-path fixtures/rsi-meta/cycle-probe/Cargo.toml
```

The ratio threshold deliberately leaves scheduler and hardware headroom; it is
intended to catch a return to registry-wide cycle scans, not small performance
changes. Results apply only to the executing host.
