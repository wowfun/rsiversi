Read the [product architecture](docs/architecture.md), [security boundary](docs/security.md), and [testing policy](docs/testing.md) before changing `rsi-agent` behavior.

- `AgentHost` is the only public runtime façade. Keep admission coordination, per-session tasks, the SQLite writer thread and bounded cold reads, model/tool adapters, request derivation, and recovery machinery private; introduce a public seam only for a real second implementation.
- The committed transcript is the source of truth. Persist model-visible content before use, derive requests only from committed events, and durably record tool dispatch before invoking a tool.
- Isolate corruption proven to belong to one closed session and transient read contention from the global health latch. Fail the whole host closed only when store identity, schema, write durability, or worker supervision is no longer trustworthy.
- Keep provider-specific syntax outside this product. `rsi-ai-protocol` owns AI semantics; `rsi-agent-protocol` owns only the tool service contract; schemas own exact external JSON shapes.
- Native service providers share the `rsi-meta` host process and trust domain. Do not describe capability routing as process isolation or a sandbox.

## Product documentation (`docs/`)

- This subtree owns current behavior spanning both `rsi-agent` packages. Package-local Rust API details belong in the owning package README or rustdoc.
- [`docs/architecture.md`](docs/architecture.md) owns execution, transcript, bounds, and recovery semantics. [`docs/security.md`](docs/security.md) and [`docs/testing.md`](docs/testing.md) own their named product concerns.
- Preserve the one-way product boundary: `rsi-agent` consumes the public `rsi-meta` interface, while `rsi-meta` remains unaware of agent concepts.
