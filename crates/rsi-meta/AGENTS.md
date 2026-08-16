Read [the product architecture](docs/architecture.md), [security boundary](docs/security.md), and [testing policy](docs/testing.md) before changing `rsi-meta` behavior.

- `CompositionHost` is the only embedded product interface. Keep registry, routing, persistence, and loader authorities private; introduce a seam only for a real second implementation.
- Keep unsafe code out of `core` and `cli`. `plugin` and `loader` are the deliberate ABI/loading exceptions; document every unsafe operation contract there.

## Product documentation (`docs/`)

- This subtree owns current behavior that spans multiple `rsi-meta` packages. Package-local API and limitations belong in the owning package README or rustdoc.
- [`docs/architecture.md`](docs/architecture.md) is an ordered execution and data-flow map. `docs/subsystems/` owns cross-package reference semantics; `docs/cookbook/` owns ordered tutorials; `docs/security.md`, `docs/development.md`, and `docs/testing.md` own their named product concerns.
- Keep [`composition-runtime.md`](docs/subsystems/composition-runtime.md), [`protocols.md`](docs/subsystems/protocols.md), and [`configuration.md`](docs/subsystems/configuration.md) mutually scoped. Link schemas and package READMEs for exact fields or local contracts.
