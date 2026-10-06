# RSI product architecture

The standard [`rsi`](../README.md) product owns Base composition,
applications, and the single local Service Host for one standard
`HostPaths` identity. Its library owns product factories, product-owned Profile
fragments, explicit metadata-driven Profile management, the transport-independent
Session domain plugin, and local/Unix-domain-socket adapters. The application-owned
[catalog](../../../apps/catalog/README.md) supplies official Application Profiles
and factories to native clients and their daemon. Root-level `apps/` owns executable
entrypoints, concrete application composition and product publication; reusable
`crates/` components never depend back on those applications. The [terminal application](../../../apps/terminal/README.md)
owns native application parsing, terminal interaction and application signals.
Its resident TUI keeps Session controllers and the terminal descriptor while an
ordinary child Profile selects linked or independently built native presentation
code. Both consume the same pure scene and cell-frame contracts.
The [Serve application](../../../apps/serve/README.md) owns authenticated HTTP configuration and signals, consuming
an independently composed service generation within that same Runtime.
The shared [GUI library](../gui/README.md) owns Rust Session controllers, projections and
submission reconciliation. The [Web Worker](../../../apps/web-worker/README.md) runs it with Meta and Profiles in a
Dedicated Worker; the [Web document](../../../apps/web/README.md) owns presentation and persistent editing.
The catalog injects the expected build family into the [Web assets plugin](../web-assets/README.md),
which supplies validated, retained resource bytes and renderer graphs to the HTTP listener.
The Worker owns API-backed renderer generation leases;
the document owns dynamic module mounting and disposal.
The [UI contribution plugin](../ui/README.md) owns bounded, generation-bound
surfaces, actions and presentation models shared by native and Worker adapters.
Its protocol is renderer-neutral; API and Portable adapters preserve the owning
UI registry's presentation identities, model revisions and invocation authority.
It uses actual application/surface Local mappings and owns no Session or layout.
The [CLI application](../../../apps/cli/README.md) owns launcher and management parsing, explicit daemon process control,
process signals, and construction of the Tokio runtime. The Agent Kernel remains the sole durable session state-machine
owner; the product Host adds live multiplexing and process ownership without
moving Agent semantics into a wire adapter. [Applications and Sessions](subsystems/applications-and-sessions.md)
explains the connection and attachment boundaries; [Host lifecycle](subsystems/host-lifecycle.md)
defines deployment identity, process leases and teardown.
The standard [Host Goal controller](../goal/README.md) and
[Schedule controller](../schedule/README.md) drive independently armed
continuations through the Session bridge. Reading durable state or reopening an
application does not recreate either scheduling authority. Agent owns the
[Program runtime](../../rsi-agent/program/README.md) and durable workflow
lifecycle; the standard product supplies its native Process and Jobs composition.

The opt-in Linux daemon [Automation owner](../automation/README.md) issues fresh,
bounded protected Goals from signed deployment events and standing operator
rules. It owns a dedicated admission ledger and queue independently of Agent
durability and process-local Jobs. The [Browser owner](../browser/README.md)
consumes Process, Sandbox and Retrieval's public destination validation for
isolated checking and private MCP attachment. Its rule policy remains above
Agent and Meta; results and public Session reads share the immutable Header's
product protection scope. Reopening an application or reading history issues no
standing execution authority.
