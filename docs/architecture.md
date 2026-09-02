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
Commands, Approval, Sandbox, Process, Shell, Jobs, Apply-Patch, Workspace, Permission Presets, and derived
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
Store adapters. It also owns bounded preset discovery/authoring and immutable
per-preset composition generations. Global providers create unpublished Tool
catalog stages; Agent-only contribution plugins register through a write-only
registrar, and one sealed catalog plus its hidden Scope form the exact
generation pin retained by drafts, resident sessions, and delayed Tool work.
Runtime-composed implementations are independent ordinary plugins; protocol
and test-support packages are libraries.

The standard [`rsi`](../crates/rsi/README.md) product owns Base composition,
Session applications, and the single local Session Host for one standard
`HostPaths` identity. Its library owns product factories, product-owned Profile
fragments, Application and Host Profile catalogs, the transport-independent
Session interface, and local/Unix-domain-socket adapters. The binary owns CLI
parsing, terminal interaction, explicit daemon process control, process
signals, and construction of the Tokio runtime. The Agent Kernel remains the sole durable session state-machine
owner; the product Host adds live multiplexing and process ownership without
moving Agent semantics into a wire adapter.

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
