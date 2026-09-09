# rsi-agent-composition

`rsi-agent-composition` owns the standing, process-local builder for immutable
Agent generations. It compiles the current `<preset>/agent.profile.toml`,
resolves every Profile factory against a frozen Agent-only contribution
allowlist, and mutates the Runtime only after that complete preflight succeeds.
Its default-preset query acquires the current snapshot and delegates to its
`AgentPresetCatalog` and default-store adapter, exposing only the validated
effective identity. A later build acquires its own snapshot.

`AgentGenerationRootFactory` supplies an explicit process-local Context for
generation ownership. It is an ordinary plugin in the containing service Profile
and preserves that service's Local isolation. Composition providers require this
root and never reacquire `Runtime::root()`. The root must outlive those providers
and their pins; retiring it disposes its complete generation subtree.

Each successful generation is built below a hidden Scope within that root. Its
pin controls reclamation independently of the composition provider Fiber. A generation first
activates private Tool and domain registrars, then activates every static Profile leaf,
requires the generation's unique `ModelContextBuilder`, seals its unpublished
Tool and typed domain catalogs, and only then becomes current. Domain registration
uses exact Meta credentials and leases; rollback closes admission even for a
registrar retained outside the failed candidate. A sealed pin preserves its
definitions across generation replacement. The builder's Local identity is
isolated with the registrar: an ancestor provider cannot satisfy a missing
selection. Multiple providers conflict at ordinary Local publication. The same
pin retains both builder and Tools for execution and delayed checkpoint work. A
missing, malformed, or otherwise failed current source returns an error; it
never falls back to an older cached generation. Failure leaves any previously
published generation unchanged. Profile compilation uses a non-cancellable
blocking task; that task itself retains the per-preset singleflight guard and
global build permit, so dropping its waiter cannot admit overlapping compilers
or let provider shutdown pass them. Once compilation returns, dropping a build
waiter cancels its detached activation, and both guards remain held until the
unpublished Scope has rolled back. Stage, Fiber, catalog, and build capacity
therefore cannot escape caller cancellation.

Builds are singleflight per preset. Each admitted build obtains exactly one
`AgentCompositionSnapshot` from its application-owned `AgentCompositionSource`
before compilation. The snapshot pairs the preset compiler/allowlist with the
exact contribution catalog. Compilation, resolution and activation retain that
snapshot even if the source publishes a replacement meanwhile. A failed snapshot
query fails selection before Runtime mutation, including a cache hit.

An unchanged Profile program and catalog identity reuse the published generation.
Catalog identity includes every linked revision/native artifact digest and update
mode, exact nominal Local/event bindings, and declared fresh Portable service
keys. The pin digest combines the compiled program with this catalog identity;
nominal Rust identities are process-local and have no cross-build encoding
guarantee. Cache equality also compares the complete typed catalog signature.
Replacing a factory object without changing its declared executable identity is
not a code update. Applications must publish a new linked revision or native
artifact identity when its implementation changes.

At most eight builds run across presets and at most 256 live preset rows exist.
A row with no current generation and no in-flight waiter is evictable under admission pressure, so
failed identities cannot permanently consume the row budget. Recompiling a
changed source publishes a new generation without disturbing existing pins.
The superseded Scope is disposed asynchronously after its final pin is
released. That Scope owner retains the complete selected catalog through cleanup,
including factories not activated by this particular Profile. Provider shutdown stops admission, cancels and joins unpublished
builds, removes its current-generation owners, and waits for every external pin
to release before disposing and joining the corresponding Scope.

`AgentContributionCatalog` holds exact `ResolvedFactory` values and nominal
Local/event markers selected by the application. Marker declaration is explicit,
bounded to 4,096 entries per lane, and frozen when the catalog enters the
immutable snapshot. Repeating the same nominal marker is idempotent; assigning
the same key to another marker is rejected. Profile owns fresh/named allocation
and generation namespaces; the catalog performs only nominal resolution.
Standard Agent addon marker declarations populate this same catalog. Catalogs
contain at most 4,096 factories. Explicit Portable generation-isolation keys use
the same per-lane entry bound and contain 1–256 UTF-8 bytes. Each hidden Scope
allocates fresh mappings for these keys before activating any leaf, so overlapping
old/new native providers and their bridges resolve inside their own generation.
Profile group mappings can further isolate an explicitly selected subtree.
`AgentCompositionFactory` is an ordinary
Meta plugin: its static constructor freezes the concrete preset catalog and
allowlist; `with_source` accepts the application-owned snapshot source. Both
receive the Scope root, while activation requires the existing
`ToolCatalogProviderContract` and explicit `AgentGenerationRootContract`.
It supplies `AgentCompositionContract`; it does
not expose preset locations, the Tool registrar, a mutable Host catalog, or a
resolver to Agent consumers.
