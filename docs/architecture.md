# RSIversi monorepo architecture

## Product ownership

The repository is a product family, not one product split into packages. A product owns an independently usable capability, current contract, architecture, trust boundary, and verification policy. The root [product table](../README.md#products) is the human-maintained entry point; the product subtree owns the facts behind each entry.

The only implemented product is [`rsi-meta`](../crates/rsi-meta/README.md), a trusted native plugin composition platform. It deliberately does not own a model client, agent loop, model-visible history, tools, or user interface. A future agent product belongs in a separate product layer above the platform rather than in the `rsi-meta` kernel.

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

The root Cargo workspace discovers `crates/*/*`. A product `docs/` directory at that depth is explicitly excluded because it is not a Cargo package. Standalone plugin and fixture workspaces retain their own manifests and lockfiles and are exercised by the owning product's conformance workflow.

## Documentation and decisions

The root README navigates products. A product README states the product's current contract and links its components. Product docs own cross-package system behavior. Package READMEs own one Cargo package. Schemas and source own exact declarations. [Agent Notes](../.agents/notes/README.md) own durable decision rationale.

Each subtree `AGENTS.md` contains only rules introduced by that boundary. Descendants inherit their ancestors and link to the authoritative rule instead of copying it.
