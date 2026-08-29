# RSIversi monorepo architecture

The repository is a family of independently usable products. Each product owns its public contract, trust boundary, and verification policy; the root owns only one-way dependency direction and workspace governance.

[`rsi-meta`](../crates/rsi-meta/README.md) is the lowest runtime layer. It owns
one `Runtime -> Context -> Fiber` lifetime graph, direct typed Local contracts,
and generation-fenced Portable contracts. Native ABI, Profile loading, and
application composition remain outside core.

[`rsi-meta-profile`](../crates/rsi-meta/profile/README.md) owns the bounded,
ordered Profile program, pure expression preflight, source watching, live
convergence, and typed local control plane. [`rsi-host`](../crates/rsi-host/README.md)
is the generic static composition SDK: it freezes explicit factory and marker
catalogs plus the Profile environment, then bootstraps exactly one Profile
without owning product implementations or introducing a second runtime.

Base capability families own Storage, Settings, Credentials, Media, Tools,
Commands, Approval, Sandbox, Jobs, Workspace, Permission Presets, and derived
projections. Their protocols and deterministic test support are libraries;
stateful providers, registries, schedulers, and policy implementations are
ordinary `rsi-meta` plugins. `rsi-meta` and `rsi-host` do not know those
products, and `rsi-host` does not select a default implementation.

[`rsi-ai`](../crates/rsi-ai/README.md) owns provider-neutral Language and Image
contracts, exact routing, provider authoring, and transports. Routers and
provider implementations export ordinary plugin factories; no family-level
Meta adapter owns their lifecycle.

[`rsi-agent`](../crates/rsi-agent/README.md) owns durable session, turn, and
Store contracts, the session Kernel, context construction, execution, and
Store adapters. Runtime-composed implementations are independent ordinary
plugins; protocol and test-support packages are libraries.

The standard [`rsi`](../crates/rsi/README.md) product owns Base composition and
the selection and assembly of Headless composition. The library assembles
product factories with product-owned Profile fragments; the binary owns CLI
parsing and the Tokio runtime.

Dependencies point from the standard product through product implementations
and protocols toward `rsi-meta`; foundation packages never depend back on a
composition or application package. A product may consume another product's
typed contract, but it may not acquire a privileged lifecycle adapter.

Product components live at `crates/<product>/<component>`. Non-crate assets use the matching namespace below `schemas/`, `plugins/`, `fixtures/`, or `examples/`. Repository-only tools live below `crates/tools/`. Current behavior is documented at its owning product or package rather than duplicated at root.

Workspace crates share one `serde_json` policy from the root dependency table:
object insertion order and exact JSON number text are preserved. No leaf crate
may change those process-wide Cargo features, because feature unification must
not make `Value` equality, hashing, or round trips depend on the selected build
graph.
