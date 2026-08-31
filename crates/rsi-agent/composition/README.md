# rsi-agent-composition

`rsi-agent-composition` owns the standing, process-local builder for immutable
Agent generations. It compiles the current `<preset>/agent.profile.toml`,
resolves every Profile factory against a frozen Agent-only contribution
allowlist, and mutates the Runtime only after that complete preflight succeeds.
Its default-preset query delegates to the same frozen `AgentPresetCatalog` and
current default-store adapter used for generation resolution, exposing only the
validated effective identity.

Each successful generation is built below a hidden Runtime-root Scope whose
owner is the generation pin, not the provider Fiber. A generation first
activates a private `ToolRegistrar`, then activates every static Profile leaf,
then seals its unpublished Tool catalog, and only then becomes current. A
missing, malformed, or otherwise failed current source returns an error; it
never falls back to an older cached generation. Failure leaves any previously
published generation unchanged. Profile compilation uses a non-cancellable
blocking task; that task itself retains the per-preset singleflight guard and
global build permit, so dropping its waiter cannot admit overlapping compilers
or let provider shutdown pass them. Once compilation returns, dropping a build
waiter cancels its detached activation, and both guards remain held until the
unpublished Scope has rolled back. Stage, Fiber, catalog, and build capacity
therefore cannot escape caller cancellation.

Builds are singleflight per preset, and an unchanged source digest reuses its
published generation. At most eight builds run across presets and at most 256
live preset rows exist. A row with no current
generation and no in-flight waiter is evictable under admission pressure, so
failed identities cannot permanently consume the row budget. Recompiling a
changed source publishes a new generation without disturbing existing pins.
The superseded Scope is disposed asynchronously after its final pin is
released. Provider shutdown stops admission, cancels and joins unpublished
builds, removes its current-generation owners, and waits for every external pin
to release before disposing and joining the corresponding Scope.

`AgentContributionCatalog` exposes only exact immutable `ResolvedFactory`
values selected by the application. `AgentCompositionFactory` is an ordinary
Meta plugin: its constructor receives the concrete preset catalog, compiler,
allowlist, and Scope root, while activation requires only the existing
`ToolCatalogProviderContract`. It supplies `AgentCompositionContract`; it does
not expose preset locations, the Tool registrar, a mutable Host catalog, or a
resolver to Agent consumers.
