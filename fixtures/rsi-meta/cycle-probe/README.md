# rsi-meta-fixture-cycle-probe

This release-mode probe exercises the public `Runtime`, `Context::apply`, and
pending-Fiber snapshot interfaces. It doubles a large population of unrelated
Fibers that all require one permanently missing service, then builds an equally
scaled public `Context::isolate` chain. A separate 4,096-Fiber pressure case
withdraws one provider from 256 consumers that each hold 256 bindings (65,536
dependency edges). It prints the dense withdrawal time, reconciliation and
single scheduler-worker high-water marks, and maximum concurrent public-snapshot
latency as a global-lock contention proxy. The provider holds a cleanup effect
after it enters `Unloading`, and the sampler must observe that exact public
state before the effect is released. A pre-poll task flag therefore cannot
satisfy the overlap assertion. It fails when observed growth becomes
superlinear, the scheduler exceeds its configured transition bound, creates
more than its one Runtime-owned worker, or a broad absolute deadline is
exceeded. This guards the reachable-only declaration index, bounded scheduler,
and persistent copy-on-write Context contracts without a checked-in
machine-specific timing baseline.

Run the probe from the repository root:

```sh
cargo run --release --locked --manifest-path fixtures/rsi-meta/cycle-probe/Cargo.toml
```

Linux CI runs this release command in addition to linting the standalone
fixture. Other platform results are not inferred from that evidence.

The ratio and latency thresholds deliberately leave scheduler and hardware
headroom; they are intended to catch architectural regressions, not small
performance changes. Results apply only to the executing host.
