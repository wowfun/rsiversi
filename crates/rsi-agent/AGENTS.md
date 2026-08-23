Read the [product architecture](docs/architecture.md), [security boundary](docs/security.md), and [testing policy](docs/testing.md) before changing `rsi-agent` behavior.

- `rsi-agent-protocol` is the only active package in this product boundary.
- No runtime or coding-tools implementation is retained. Do not restore the removed `CompositionHost` integration or treat historical source as a current contract.
- A future runtime must consume the `Runtime -> Context -> Fiber` foundation through ordinary plugins and define durability as a separate product-owned boundary.
- Keep provider-specific syntax outside this product. `rsi-ai-protocol` owns AI semantics; `rsi-agent-protocol` owns only the tool service contract.

## Product documentation (`docs/`)

- This subtree owns the active protocol contract and the boundary for a future foundation-based runtime. Package-local Rust API details belong in the owning package README or rustdoc.
- [`docs/architecture.md`](docs/architecture.md), [`docs/security.md`](docs/security.md), and [`docs/testing.md`](docs/testing.md) own the current product boundary.
- Preserve the one-way product boundary: `rsi-agent` consumes the public `rsi-meta` interface, while `rsi-meta` remains unaware of agent concepts.
