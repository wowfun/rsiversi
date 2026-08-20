# RSIversi monorepo architecture

## Product ownership

The repository is a product family, not one product split into packages. A product owns an independently usable capability, current contract, architecture, trust boundary, and verification policy. The root [product table](../README.md#products) is the human-maintained entry point; the product subtree owns the facts behind each entry.

The implemented products are ordered by dependency:

1. [`rsi-meta`](../crates/rsi-meta/README.md) is the trusted native plugin composition platform. It owns service composition, routing, and provider lifecycle, but not agent semantics.
2. [`rsi-ai`](../crates/rsi-ai/README.md) is the provider-neutral AI integration product. It owns five semantic capability contracts, standalone exact routing, concrete provider adapters, and their `rsi-meta` service wrapper, but not agent history or provider composition policy.
3. [`rsi-agent`](../crates/rsi-agent/README.md) is the durable agent-turn runtime. It consumes `rsi-ai` and tool services through `rsi-meta`, owns model-visible history, retry scheduling, artifact durability, and loop semantics, and deliberately leaves provider routing and user interfaces outside its boundary.

The layer boundary is one-way: `rsi-ai` uses `rsi-meta` only in its adapter package, while `rsi-agent` consumes the public `rsi-ai` protocol and `rsi-meta` host interface. `rsi-meta` has no knowledge of agents, transcripts, AI semantics, or tools. Concrete AI provider packages do not depend on `rsi-agent`.

## Source and asset layout

Product components live at `crates/<product>/<component>`. Cargo package names remain globally meaningful even when component directory names are short. Repository-only tools live under `crates/tools/`.

Non-crate assets stay in repositories of their own kind and use a product namespace:

```text
schemas/<product>/
plugins/<product>/
fixtures/<product>/
examples/<product>/
```

This keeps schemas, standalone workspaces, conformance artifacts, and runnable examples discoverable by kind without making the root their behavioral owner. Each product namespace links back to its product instructions.

The root Cargo workspace discovers `crates/*/*`. A product `docs/` directory at that depth is explicitly excluded because it is not a Cargo package. Each standalone plugin or fixture workspace retains one manifest and lockfile and is exercised by the owning product's conformance workflow; a product namespace may group several cooperating fixture packages into one such workspace.

## Documentation and decisions

The root README navigates products. A product README states the product's current contract and links its components. Product docs own cross-package system behavior. Package READMEs own one Cargo package. Schemas and source own exact declarations. [Agent Notes](../.agents/notes/README.md) own durable decision rationale.

Each subtree `AGENTS.md` contains only rules introduced by that boundary. Descendants inherit their ancestors and link to the authoritative rule instead of copying it.
