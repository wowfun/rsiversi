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
Commands, Approval, User Questions, Sandbox, Process, PTY, Shell, Jobs, Apply-Patch, Workspace, Files, Permission Presets, and derived
projections. Their protocols and deterministic test support are libraries;
stateful providers, registries, schedulers, and policy implementations are
ordinary `rsi-meta` plugins. `rsi-meta` and `rsi-host` do not know those
products, and `rsi-host` does not select a default implementation.
The [native Files library](../crates/rsi-files/native-fs/README.md) supplies shared
directory-handle mechanics; callers retain their own trust and authorization policy.

[`rsi-pty`](../crates/rsi-pty/README.md) owns live terminal scopes, bounded ANSI/VT100
screens and attachment/controller state. Process owns the native PTY, byte I/O
and reaping. Sandbox owns its explicit restricted PTY plan. The standard product
binds terminal scopes to persisted Session authority and service-generation
lifetime; the Agent Kernel and durable Session format do not own terminals.

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

[`rsi-mcp`](../crates/rsi-mcp/README.md) owns operator-selected external protocol
servers, finite frozen catalogs and connection retirement. It consumes Process,
Sandbox, Credentials, Tools and generic Agent composition seeds.
[`rsi-retrieval`](../crates/rsi-retrieval/README.md) owns model-selected public-web
URL policy, bounded retrieval and attributed source results. Their configuration
and model adapters remain above generic Agent and Meta contracts; neither moves
network policy into Kernel or Context.

[`rsi-acp`](../crates/rsi-acp/README.md) owns bounded stable ACP wire validation,
correlation and drain over explicitly supplied byte transports. Native Agent
durability remains with Kernel; subprocess confinement and reaping remain with
Sandbox and Process. External-agent observations do not become native Agent Facts.

The optional [`rsi-lsp`](../crates/rsi-lsp/README.md) source addon owns bounded
read-only language-server queries over Process, Sandbox and Files. Its Tool and
the product's [language UI](../crates/rsi/lsp-ui/README.md) consume the same semantic
results; the foundation addon does not depend on product clients or UI.

The standard [`rsi`](../crates/rsi/README.md) product owns application composition,
the local Service Host, Session adapters and native and browser clients.
Its [product architecture](../crates/rsi/docs/architecture.md) defines their
ownership and connections. It consumes foundation contracts without moving
Agent durability or capability policy into presentation or transport.

Dependencies point from the standard product through product implementations
and protocols toward `rsi-meta`; foundation packages never depend back on a
composition or application package. A product may consume another product's
typed contract, but it may not acquire a privileged lifecycle adapter.

Reusable product components live at `crates/<product>/<component>`. The standard
RSI product's executable applications and their explicit catalog live at
`apps/<app>`; application packages consume product libraries, never the reverse,
including build and test dependencies. Runtime plugin identity does not determine
source placement. Assets compiled into a library live with that library;
standalone schemas, fixtures and examples retain their product namespaces.
Repository verification tools live below `crates/tools/`; application development
and distribution belong to the application layer. Current behavior is documented
at its owning product or package rather than duplicated at root.

Workspace crates share one `serde_json` policy from the root dependency table:
object insertion order and exact JSON number text are preserved. No leaf crate
may change those process-wide Cargo features, because feature unification must
not make `Value` equality, hashing, or round trips depend on the selected build
graph.
