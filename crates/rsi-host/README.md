# rsi-host

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
