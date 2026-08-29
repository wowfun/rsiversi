Read the [product architecture](docs/architecture.md), [security boundary](docs/security.md), and [testing policy](docs/testing.md) before changing `rsi-agent` behavior.

- The session protocol, kernel, SQLite store, executor, and tool protocol are
  independent modules. Keep their interfaces narrow and do not move the
  product state machine into a wire or Store adapter.
- Runtime integration consumes `Runtime -> Context -> Fiber` through ordinary
  plugins. Do not restore the removed `CompositionHost` integration or treat
  historical source as a current contract.
- Keep provider-specific syntax outside this product. `rsi-ai-protocol` owns AI
  semantics; `rsi-tools-protocol` owns Tool contracts and retained results.

## Product documentation (`docs/`)

- This subtree owns the durable-session and tool contracts. Package-local Rust
  interface details belong in the owning package README or rustdoc.
- [`docs/architecture.md`](docs/architecture.md), [`docs/security.md`](docs/security.md), and [`docs/testing.md`](docs/testing.md) own the current product boundary.
- Preserve the one-way product boundary: `rsi-agent` consumes the public `rsi-meta` interface, while `rsi-meta` remains unaware of agent concepts.
