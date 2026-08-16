# RSIversi

RSIversi is a pre-release monorepo for independently usable runtime products.

## Products

| Product | Purpose | Documentation |
|---|---|---|
| `rsi-meta` | Trusted native plugin composition platform | [Product overview](crates/rsi-meta/README.md) |

## Repository map

- [Architecture](docs/architecture.md) explains product ownership, source layout, and cross-product boundaries.
- [Agent Notes](.agents/notes/README.md) preserve decision rationale and lifecycle.
- [`crates/`](crates/) contains product components and repository tools.
- [`schemas/`](schemas/), [`plugins/`](plugins/), [`fixtures/`](fixtures/), and [`examples/`](examples/) group non-crate assets by owning product.
- [Repository instructions](AGENTS.md) define the standing development and documentation rules.

Start with a product overview for setup, usage, security, development, and verification.
