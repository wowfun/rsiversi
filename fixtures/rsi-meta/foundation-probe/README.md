# rsi-meta-fixture-foundation-probe

This release-mode fixture is black-box evidence for the public `rsi-meta`
foundation. It exercises only `Runtime`, `Context`, `PluginFactory`,
`FiberHandle`, dynamic-supply, snapshot, effect, and resource APIs.

The deterministic lifecycle case proves that a supply created during Loading is
self-visible to its provider but remains externally absent: the consumer keeps
one exact `MissingService` reason and activation is not called. Publishing the
generation activates the consumer, exact supply withdrawal returns it to
Pending, and an Active-generation re-provision activates it again with a new
`SupplyId`. Provider disposal is held inside an owned cleanup effect while the
public snapshot must show `Unloading`; final shutdown must leave every reported
Runtime resource at zero.

The scalability cases double 1,024, 2,048, and 4,096 unrelated Fibers with one
actually missing requirement and build equally scaled persistent
`Context::isolate` chains. A separate pressure case fills the configured Fiber
and service bounds with 4,096 Fibers, 256 actual service supplies, 256 consumers,
and 65,536 retained requirement edges. Its dependency-edge limit also reserves
bounded headroom for old and replacement attempts to coexist during v2
preparation. The next Fiber is observably rejected at the configured bound.
Withdrawing one provider must leave every dependent Pending on the exact missing
service while a sampler overlaps the provider's explicitly blocked `Unloading`
effect. The fixture records resource high-water marks, the single Runtime-owned
scheduler worker, and maximum public-snapshot latency.

Elapsed times, scaling ratios, and snapshot latency are report-only diagnostics;
host scheduling pauses make them unsuitable as conformance pass/fail evidence.
The fixture gates deterministic lifecycle, resource, capacity, and scheduler
worker invariants. Its explicit timeouts are liveness watchdogs, not performance
baselines, and all timing observations apply only to the executing host.

Run from the repository root:

```sh
cargo metadata --locked --offline --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml --format-version 1 --no-deps
cargo fmt --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml --check
cargo clippy --locked --offline --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml --all-targets -- -D warnings
cargo run --release --locked --offline --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml
```

The conformance command runs the release probe on Linux and substitutes a
locked offline release build on other hosts. Only the Linux execution is
runtime evidence for this fixture, and native Windows or macOS behavior is not
inferred from compile-only checks.
