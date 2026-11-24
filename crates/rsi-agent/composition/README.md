# rsi-agent-composition

Concurrent preparation callers for the same preset share one in-flight source
snapshot and exact-byte probe. The shared result expires with its waiters; later
request boundaries recheck source bytes. File timestamps alone are not source
identity. The global blocking-work bound remains held until each admitted task
finishes, even if all its callers cancel.

An application may supply a preset absent from all native discovery roots.
Composition asks its active provider for the named preset and compiles the
returned in-memory plugin entries with the same frozen compiler and Agent
allowlist. An invalid native definition keeps precedence. A missing or retired
provider cannot authorize a new application-only preset. Filesystem authoring
and native configuration documents continue to describe their original sources.
Application-supplied presets require the Host-local workspace and a live
application input preparer. Selecting one for an SSH Session reports that an
explicit native preset is required. This leaves the Host default intact.
SSH lookup checks the native roots and returns before contacting the application.

Published pins retain current executable owners and a diagnostic selection digest.
Cold preparation resolves current preset sources and code, validates retained state
for current Domains, and initializes new Domains. It never loads historical
executable recipes. See the [generation contract](../docs/subsystems/composition-generations.md).

Every generation declares its current Tool output catalog, including an empty catalog.
Historical typed outputs retain their saved declaration in Domain history.

The generation also captures its optional native prompt assembler and exact ScopeKey.
Its Local marker is isolated with the Context builder: an ancestor cannot substitute
for the selected generation. A pin retains the assembler through awaited preparation
and replacement, and the last pin releases the same generation ownership. Passive
prompt registrations belong to their registering plugin's effects, not extra Fibers.

Session-specific composition remains an explicit application-owned input. Before
ordinary preset resolution, `AgentCompositionSource::session_pin` may return an
already prepared exact pin for a validated Header and optional saved baseline.
It performs no activation or I/O; its owner validates the Session/workspace and
baseline binding, bounds retained pins, and rejects unavailable private inputs
instead of falling back to a global catalog. Generic callers use
`AgentComposition::pin_session`; plain preset discovery continues using `pin`.
The resulting pin must still match the Header preset. This permits a protocol
application to finish private provider preparation before draft publication,
without teaching Kernel or generic composition about external protocol syntax.
The source owner may derive a private snapshot with another explicit preset
catalog, exact factory replacements and a validated seed. Replacements preserve
all nominal marker and isolation declarations, reject duplicate replacement IDs
and retain the normal factory bound. The derived snapshot is independent of the
standing source and is never installed there implicitly.

`rsi-agent-composition` owns the standing, process-local builder for immutable
Agent generations. It compiles the current `<preset>/agent.profile.toml`,
resolves every Profile factory against a frozen Agent-only contribution
allowlist, and mutates the Runtime only after that complete preflight succeeds.
Its default-preset query acquires the current snapshot and delegates to its
`AgentPresetCatalog` and default-store adapter, exposing only the validated
effective identity. A later build acquires its own snapshot.

An embedder can provide a frozen snapshot/source directly, or construct
`AgentCompositionFactory::from_source_contract` to consume an ordinary
`AgentCompositionSourceContract` service. The latter declares a hard Local
dependency during prepare and captures the exact supplied source during
activation. No composition service is published before that source is available.
The source provider owns staging and update supervision; this builder still
captures exactly one immutable compiler/catalog pair per admitted Agent build.

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

Pins from one published generation share its immutable base catalog measurement.
Claim-specific membership changes retain their own measurements.

An unchanged evaluated Profile, catalog, code selection and seed identity reuse the published
generation. Each preset retains its last compiled tree and selection digest;
unchanged source bytes skip parsing, evaluation, freezing and selection hashing.
The source probe uses a separate eight-operation blocking lane to verify current
admission and exact included-file bytes. Prepared candidates retain no build
permits or preset lock; consuming them acquires the preset's build lock and one
of eight build permits. An unchanged source probe cannot queue behind a retained
candidate or plugin activation. Each preset retains separate current slots for fresh, restoring and replacing
inputs, so alternating draft creation and cold resume does not reactivate both
generations. A new program or executable catalog retires the cached slots;
changing a seed replaces only its fresh/restoring slot. External pins retain
their existing independent lifetime.
Catalog identity includes every linked revision/native artifact digest and update
mode, exact nominal Local/event bindings, and declared fresh Portable service
keys. The process-local pin digest combines the evaluated program and selection with this catalog identity;
nominal Rust identities are process-local and have no cross-build encoding
guarantee. Cache equality also compares the complete typed catalog signature.
Replacing a factory object without changing its declared executable identity is
not a code update. Applications must publish a new linked revision or native
artifact identity when its implementation changes.

At most eight builds run across presets and at most 256 live preset rows exist.
A row with no in-flight waiter or build is evictable in least-recently-used order
under admission pressure, including successful current generations. External
pins independently retain their exact generation; eviction only releases cache
ownership. A later lookup of an evicted identity can build a new generation.
Recompiling a
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
An explicit `isolate_local_contract` declaration also registers its nominal marker
and gives that service a fresh mapping in each hidden generation Scope. This is
for generation-owned providers such as the prompt authoring registry; shared Host
dependencies retain their inherited mapping. The declaration participates in cache
identity, and private snapshot derivation preserves it.
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

Before cache lookup, each build captures the selected generation seed together
with the source catalog. Cold selection overlays the durable baseline on current
seed defaults: retained state wins, while newly declared owners receive current
initial inputs. The generation digest includes the exact
seed SHA-256 and fresh/restoring mode; a saved external manifest therefore cannot reuse a
catalog built for another manifest. The immutable seed is supplied inside the
same fresh Local isolation as the registrars, before any Profile leaf activates.
Unchanged definitions may share a pin only when these inputs also match.
An owner-detected seed codec mismatch survives failed activation as a closed,
generation-local diagnostic. It reports typed saved/expected identities and the
new-conversation action after rollback. Other Profile preparation, activation and
Pending failures identify the validated Profile instance and failure stage;
plugin-owned diagnostic text is not copied into that error. A recorded mismatch also prevents sealing if a plugin
ignores its failed state lookup.

After Tools seal, composition freezes their exact-name output declarations in
the `rsi.tools.outputs` Domain baseline (codec 1), then seals Domains. That
registration uses the hidden Scope's ordinary Meta credential and is withdrawn
with a failed candidate. Historical consumers read this baseline; current Tool
registries never reinterpret a saved result. Cold recovery validates an existing
catalog encoding and publishes the current declarations for future Tool results.
Unknown codecs reject recovery; changed declarations or a newly introduced catalog
do not require a new conversation. History attachment remains independent of
executable providers.

A valid executable source snapshot may explicitly mark only its current generation
inputs unavailable with a static, redacted reason supplied by the source owner.
Fresh builds report that reason before cache reuse. Restoration can
still use its supplied saved seed with that executable catalog; unavailable code,
compiler or native admission continues to reject both modes. This distinction
allows an offline integration to restore saved definitions without treating a
failed live discovery as an empty catalog.
