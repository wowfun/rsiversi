# rsi-host

Generic composition accepts a frozen environment with or without filesystem
paths. `HostBuilder::without_paths` supports embedders such as browser Workers;
its Profile source is an immutable bundle or programmatic document. Native file
entry points and watchers are target-specific. The caller injects platform
Execution before starting the single Runtime. Host path access is optional and
does not invent storage authority for a path-free application.

`rsi-host` is the generic static composition SDK above
[`rsi-meta`](../rsi-meta/README.md) and
[`rsi-meta-profile`](../rsi-meta/profile/README.md). It owns an explicit per-Host
linked factory catalog, stable Local marker registration, frozen Profile
environment, Host paths, and the authority to start exactly one top-level
Profile bootstrap. It does not own Profile parsing or convergence, a second
runtime, product implementations, package discovery, or live remote control.

`HostBuilder` rejects duplicate linked `PluginId`, Local contract key, Local
event key, and linked-fragment registration before it creates a Host. Building
freezes all bootstrap input. The Host supplies an immutable resolver to its
Profile plugin, which delegates lifecycle work to the public Meta
`Runtime -> Context -> Fiber` interface.

Before startup, `Host::preview_file` can purely compile the frozen fragments,
selected source, environment, and launch patches and resolve every enabled
plugin. Preview performs no factory preparation, Runtime activation, credential
resolution, or Store lease acquisition.

Building validates and freezes Runtime policy without creating an executor or
Runtime. `HostBuilder::execution` can freeze explicit platform execution;
startup constructs the Runtime from it. Native startup without an explicit
backend captures its entered Tokio handle. Preview remains usable without Tokio.

An existing application Runtime can consume `Host::prepare_in` to prepare an
ordinary child Profile with the frozen catalog, environment and Profile limits.
Preparation borrows the frozen Host; independent child Profiles may reuse that
catalog while retaining separate control and generation ownership.
That Runtime supplies execution and global resource policy. The caller installs
the returned bootstrap through Meta in its owned Context and retains its control
handle. Scope creation, Local isolation and child disposal belong to that caller;
preparation neither creates nor shuts down a Runtime. This supports embedded
compositions under the same Runtime/Context/Fiber authority as their application.

`Host::isolate_local_context` derives a caller-supplied Context with fresh Local
identities for the frozen contract/event catalog and Profile control. It creates
no Fiber or Runtime and does not activate a provider. Unregistered contracts and
Portable identities keep the caller's mappings; explicit Profile groups own any
additional isolation. This lets a product isolate a complete child catalog
without duplicating its marker list, while retaining chosen uncatalogued parent
dependencies. The caller still owns the real child Scope and its cleanup.
This is catalog name isolation, not a capability allowlist or a security sandbox.
Supply a parent Context exposing only authority intended for the child; inherited
Portable services and uncatalogued Locals must be fenced explicitly by that caller.

Because limits remain mutable until build, build revalidates every previously
registered identifier, marker, fragment, define, and launch patch against the
final limits before creating the Runtime.

## Frozen inputs

Construction requires explicit absolute config, state, and cache paths; Host
never discovers them from the process environment. The builder also receives
bounded Host and Meta limits. Linked registrations bind one `PluginId`, build
revision, `UpdateMode`, and factory implementation. Neither Profile parsing nor
factory execution may replace that identity. Local contract and event names are
configuration keys only: the builder records their exact Rust `TypeId` and
rejects key or type duplication before any factory is prepared.

A linked Profile fragment is an immutable ordered program segment registered
under one fragment ID. Host also freezes an explicit platform name and JSON
compatible Rhai defines. There is no ambient inventory, dynamic library search,
package resolver, environment lookup, or post-build catalog mutation.

The complete Profile language, source, preflight, replay, watcher, and control
contracts live at [`rsi-meta-profile`](../rsi-meta/profile/README.md). Profile
bootstrap is an ordinary plugin Fiber, but only Host may construct it directly.
The Host does not expose its root Context or mutable Runtime.

The running Host permits typed point-of-use lookup only for Local contracts
explicitly frozen in its catalog, plus Profile's built-in `ProfileControl`.
This does not expose the root Context or lifecycle mutation authority and does
not create a managed dependency.

Shutdown delegates deterministic quiescence to Meta and returns its structured
cleanup outcome. Windows and macOS behavior is claimed only when their native
test suites run on those systems.

The SDK is usable by custom Rust applications. The standard product composition
belongs to the [`rsi` product](../rsi/README.md), not this family.

During explicit product assembly, builder `has_local_contract` and
`has_local_event` report exact marker membership without mutation. An embedder
may use these to share repeated declarations. The registering methods continue
to reject duplicates and conflicting keys; no factory code runs during lookup.
