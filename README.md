# RSIversi

RSIversi is a pre-release monorepo for independently usable runtime products.

## Products

| Product | Purpose | Documentation |
|---|---|---|
| `rsi-meta` | Context/Fiber plugin foundation with a trusted native adapter | [Product overview](crates/rsi-meta/README.md) |
| `rsi-ai` | Provider-neutral AI SDK and standalone provider integrations | [Product overview](crates/rsi-ai/README.md) |
| `rsi-agent` | Active bounded agent/tool protocol | [Product overview](crates/rsi-agent/README.md) |

## Repository map

- [Architecture](docs/architecture.md) explains product ownership, source layout, and cross-product boundaries.
- [Agent Notes](.agents/notes/README.md) preserve decision rationale and lifecycle.
- [`crates/`](crates/) contains product components and repository tools.
- [`schemas/`](schemas/), [`plugins/`](plugins/), [`fixtures/`](fixtures/), and [`examples/`](examples/) group non-crate assets by owning product.
- [Repository instructions](AGENTS.md) define the standing development and documentation rules.

Start with a product overview for setup, usage, security, development, and verification.
