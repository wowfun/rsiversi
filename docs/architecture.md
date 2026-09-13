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

[`rsi-api`](../crates/rsi-api/README.md) owns transport-independent API identities
and retained wire-buffer budgets. Its Portable adapter transfers explicitly
supplied API authority through Meta capabilities; product domains own semantic
target narrowing. Domain operations and durable semantics remain
with their owning product; the API foundation does not import those products.

Base capability families own Storage, Settings, Credentials, Media, Tools,
Commands, Approval, User Questions, Sandbox, Process, Shell, Jobs, Apply-Patch, Workspace, Files, Permission Presets, and derived
projections. Their protocols and deterministic test support are libraries;
stateful providers, registries, schedulers, and policy implementations are
ordinary `rsi-meta` plugins. `rsi-meta` and `rsi-host` do not know those
products, and `rsi-host` does not select a default implementation.
The [native Files library](../crates/rsi-files/native-fs/README.md) supplies shared
directory-handle mechanics; callers retain their own trust and authorization policy.

[`rsi-ai`](../crates/rsi-ai/README.md) owns provider-neutral Language and Image
contracts, exact routing, provider authoring, and transports. Routers and
provider implementations export ordinary plugin factories; no family-level
Meta adapter owns their lifecycle.

[`rsi-agent`](../crates/rsi-agent/README.md) owns durable session, turn, and
Store contracts, the Agent Kernel, context construction, execution, and
Store adapters. It also owns bounded preset discovery/authoring and immutable
per-preset composition generations. Global providers create unpublished Tool
catalog stages; Agent-only contribution plugins register through a write-only
registrar. The sealed catalog, selected context builder and hidden Scope form
the exact generation pin retained by drafts, resident sessions, delayed Tool
work and checkpoint maintenance.
Runtime-composed implementations are independent ordinary plugins; protocol
and test-support packages are libraries.
The [Agent Goal domain](../crates/rsi-agent/goal/README.md) owns frozen objectives,
round allocation and report settlement through those composition contracts.
Kernel continuation admission provides live authority separately from durable
state. Kernel also consumes the public Jobs status types to relay an executor's
existing claim-bound read port; Jobs scope ownership stays with the executor.

The standard [`rsi`](../crates/rsi/README.md) product owns Base composition,
applications, and the single local Service Host for one standard
`HostPaths` identity. Its library owns product factories, product-owned Profile
fragments, Application and Host Profile catalogs, the transport-independent
Session domain plugin, and local/Unix-domain-socket adapters. The terminal package
owns native application parsing, terminal interaction and application signals.
Its resident TUI keeps Session controllers and the terminal descriptor while an
ordinary child Profile selects linked or independently built native presentation
code. Both consume the same pure scene and cell-frame contracts.
The Serve application owns authenticated HTTP configuration and signals, consuming
an independently composed service generation within that same Runtime.
The shared GUI application owns Rust Session controllers, projections and
submission reconciliation. The Web adapter runs it with Meta and Profiles in a
Dedicated Worker; the document bridge owns presentation and persistent editing.
Static Web assets and complete renderer graphs are supplied by their own plugin
to the HTTP listener. The Worker owns API-backed renderer generation leases;
the document owns dynamic module mounting and disposal.
The [UI contribution plugin](../crates/rsi/ui/README.md) owns bounded, generation-bound
surfaces, actions and presentation models shared by native and Worker adapters.
Its protocol is renderer-neutral; API and Portable adapters preserve the owning
UI registry's presentation identities, model revisions and invocation authority.
It uses actual application/surface Local mappings and owns no Session or layout.
The binary owns launcher and management parsing, explicit daemon process control,
process signals, and construction of the Tokio runtime. The Agent Kernel remains the sole durable session state-machine
owner; the product Host adds live multiplexing and process ownership without
moving Agent semantics into a wire adapter.
The standard [Host Goal controller](../crates/rsi/goal/README.md) drives explicitly
armed continuation through the Session bridge. Reading Goal state or reopening
an application does not recreate that scheduling authority.

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
