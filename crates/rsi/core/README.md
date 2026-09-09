# rsi

The native application connection is an ordinary plugin: it mounts its preset
and embedded-service or UDS-client Profiles beneath its own Context, then publishes
independent application-facing domain capabilities. Terminal application factories
come from rsi-terminal. Initial Profile preparation validates application arguments
before this connection plugin activates. The launcher invokes ApplicationRun and
disposes the one enclosing Runtime.

This package implements the standard RSIversi product described by the product
[contract](../README.md). The library owns the explicit linked factory catalog,
standard composition and product-owned Profile catalogs. The Session plugin
publishes the transport-independent `SessionService`. Its trusted `SessionIngress`
is registered in the same Host catalog for server endpoint composition; remote
clients receive only the application-facing Session contract.
The binary owns launcher/management parsing, process control and the Tokio runtime.
[rsi-terminal](../terminal/README.md) owns the native application factories, their
argument grammars, renderer sinks, terminal input and task lifetime. Shared
submission reconciliation and observation cursor/retry policy live in
[rsi-client](../client/README.md).

The standard catalog links providers but does not select or enable a deployment.
A persistent Profile instantiates the intended provider, while Settings names
the exact default deployment and model. Tests can inject a credential store at
the public composition seam without consulting real user state.

The standard service Profile installs an API registry, persistent service identity,
connection negotiation and the domain endpoint plugins. Identity uses the same
exclusive native owner lease as process startup. Direct library startup acquires
that lease during activation; daemon/embedded selection injects its existing lease
and fresh epoch. These runtime values do not change the frozen catalog or launch
preview. EndpointId persists in Base Storage independently of Session history.
Daemon startup adds the ordinary Local API listener to that service Profile and
publishes metadata after the complete Profile is active. Native publication is a
process role excluded from the shared service launch key, so embedded and daemon
owners select the same desired service composition. The listener's factory is
always in the frozen native catalog; its launch-key configuration is supplied by
startup after pure service preview. Remote connections use the independent UDS
connection plugin and the domain client plugins. Detaching retires only those
client capabilities and their local transport work.

`RunningRsi::boot_host_profile_in` mounts the same standard service composition
below a caller-owned application Context. It uses that Runtime's Execution and
global limits and the shared [ScopedProfile](../application/README.md) owner,
with a real meta-scope child Fiber and fresh Local identities for the service
catalog. Shutdown disposes only that subtree. Native daemon/library root
startup retains its independent Runtime. The caller owns the parent lifecycle,
including cancellation during child bootstrap; no scoped Host owns parent shutdown.
Local selection takes this application Context for both modes: embedded service
plugins and remote connection/domain-client plugins mount isolated child Profiles.
The application disposes its connection subtree before shutting down its Runtime.

`StandardServiceDaemon::start_in` mounts the publishing daemon in a caller-owned
Runtime with the same isolated child-Profile lifetime. It consumes the exact
preacquired owner lease and publishes metadata only after the Local API listener
is active. The standard Serve application composes this service owner with an
independent authenticated HTTP application. Transport configuration does not move
domain ownership into the application or create another Runtime.

Daemon lifetime follows the owning Profile. Normal listener retirement during
Profile convergence waits for the replacement; listener failure, a stopped
Profile, or convergence without a usable listener ends serving. Parent teardown
can fence the restart-required owner before Profile cleanup reports Stopped;
that owner withdrawal is normal termination. Listener
diagnostics are observed per replacement, and shutdown resolves the current
approval broker. Neither observation nor retained application handles keep an
obsolete service generation active.

The Serve composition resolves the current dispatcher, device verifier and
administration owner at each new call. Admitted calls retain their selected
generation and are never replayed across reload. During replacement, unavailable
capabilities reject new calls. The process owner, EndpointId, HostEpoch and local
launch identity remain stable across a supported Profile reload; connection
description is therefore independent of listener replacement.
Changing only the Agent executor retains the independent API registry and local
listener under exact Profile convergence. It produces no listener replacement or
intermediate final listener diagnostic; the retained listener reports its final
diagnostics at shutdown.

`rsi --profile NAME [application arguments]` selects one named Application
Profile. The built-in `headless` application accepts one positional task or
`--stdin`; `cli` is the line-oriented interactive application. Both drive
the same `SessionService` surface, subscribe strictly after the durable
acceptance sequence, and render the subsequent live Facts. The acceptance and
terminal envelopes are terminal-owned presentation records rather than a second
Agent execution API.

The connection plugin retains its scoped preset catalog through connection
cleanup. Remote mode uses that catalog to derive the strict local launch preview;
it never owns or shuts down the independently running daemon.

Exit status 0 means a completed turn; an interactive Session also treats its
user's locally cancelled turn as a successful control action. Status 1 covers
failures after the application has resolved its Session handle and entered
runtime work, including submission-time route or provider configuration
rejection and a failed, partial, interrupted, or budget-exceeded terminal
outcome. Status 2 covers command-line, profile/catalog, Host bootstrap, and
initial Session create/attach selection failures before that handoff. Status
130 means headless signal cancellation.

`rsi agent-preset` is a management-only command family. It discovers presets
through system roots supplied by the product, then the absolute configured
`roots` in the independent `rsi.agent-presets` Settings namespace, then the
writable `<config>/agent-presets` root. The same namespace layers an optional
user `default` over the fixed deployment default `standard`; each configured
root carries an independent `system` or `user` trust label but remains
read-only. `default set` persists any syntactically valid id; current discovery,
health, and composition are checked only when a fresh session resolves that
selection. `roots` is an order-preserving array of at most 32 `{path, trust}`
objects; `path` must already be absolute, `trust` defaults to `user`, and paths
are not shell-expanded. Management commands emit text or one JSON document and
use only exit statuses 0 and 2. They do not start, switch, or compose an Agent
generation. Copy names both identities explicitly as `copy --from SOURCE --id
ID [--name NAME]`. JSON roster and show rows always expose `id`, a `metadata`
object containing `name` and `description`, independent `source` and `trust`,
flat `status` and nullable `reason`, and `default`.

The preset catalog and its Settings namespace registration belong to an ordinary
catalog plugin. AgentPresetManager observes that capability and owns its Profile
lifetime; it does not register a second Settings owner outside the plugin graph.
Withdrawal drops the namespace registration together with the catalog publication.
The catalog owner retains one bounded startup diagnostic for its management caller;
generic Profile lifecycle diagnostics continue to redact plugin error text.
`AgentPresetManager::open_standard_in` mounts that Profile below the application's
existing Context with isolated Settings/catalog identities. Standalone management
keeps an independent root Profile; the scoped manager never owns parent shutdown.

The shipped `standard` asset is materialized below a digest-addressed,
owner-writable cache, but the catalog grants System authority only to that
exact verified `standard` identity and directory. Other cache siblings are not
discovered as presets and cannot inherit System source or trust.

Addons use `StandardAddonBuilder`, immutable `StandardAddon`, and
`StandardAddonSet`. A declaration owns its exact resolved factories, Local/Event
markers, explicit Profile fragments, target platforms and descriptive factory
metadata. `register_resolved` preserves an embedder-supplied `ResolvedFactory`,
including NativeCatalog provenance; linked registration uses the same path.
Descriptions carry the complete `FactoryIdentity`, and frozen Host/Agent catalogs
and product digests retain it without substituting a linked revision. Native
values must come from the embedder's existing NativeCatalog; this declaration API
does not load artifacts. `isolate_agent_portable` declares generation-private
Portable service keys for native providers and their linked bridges, using the
Agent composition catalog's bounds. Declaration order does not affect their
identity. Installation and dynamic source selection remain separate product work.
Factories have explicit Service, Agent, Application or Client scope;
registering a factory does not instantiate it. A Profile entry or declared
fragment selects activation. Domain endpoint and client factories are declared
by their addon in their respective scopes; an endpoint is remotely exposed only
when its Profile enables it. Build-time UI assets are ordinary application
factories consuming the existing assets contract, with their build revision
included in the frozen identity. No runtime JavaScript discovery is implied.

`StandardComposition::with_addons` carries the same immutable declarations into
preview and embedded/daemon startup. The Agent compiler and contribution catalog
are derived from the declared Agent factories, including the built-in tools and
default model-context builder. Agent Profiles explicitly select one builder;
the standard preset selects `rsi.agent.context.default`. Custom presets can
select another declared builder. Missing or conflicting selections fail before
the Agent generation becomes current.
Every resulting Host catalog freezes before activation and rejects duplicate
factory/fragment identities, including collisions with built-ins. Exact repeated
marker declarations are shared; two Rust marker types claiming the same key are
rejected. Explicit unsupported platform declarations fail before activation.
Factory metadata contains a bounded summary and optional JSON configuration
schema. It is descriptive: preview never calls `PluginFactory::prepare` and
cannot establish that a configuration will activate. Prepare remains the sole
factory authority for semantic validation, normalization and requirements.
Settings retains its separate validator. Schema is never used as a second
activation validator.

`export_domain<C>` explicitly forwards the declared Local domain from the
selected embedded service or remote Client into the application. It declares
that marker in all three catalogs, and its ordinary connection Fiber owns the
publication and withdrawal. It does not turn an arbitrary Local capability into
an API: the addon must still supply and enable an endpoint and a matching client.
Missing exports fail application activation with connection cleanup retained.
Factory metadata uses the public addon byte/depth/platform/count limits; schema
traversal retains only a depth-bounded iterator stack. A preset manager records
its declaring composition identity, and attaching or later changing addon inputs
cannot silently pair an old compiler with a different contribution catalog.

Local native addon storage uses `NativeAddonStore` on Unix. Installation reads an
explicit bounded TOML manifest and a regular artifact beneath its retained source
directory, hashes the copied bytes, and publishes a content-addressed object plus
an installed record. It executes no build command, library constructor, ABI entry
or plugin. The source manifest requires `format = 1`, `id`, `plugin`, `target` and
`artifact`; optional `portable_services` declares generation-private keys.
`artifact` is a normalized relative path. Optional build metadata declares an
explicit argv, watch paths and deadline for a separate build action; none of that
command text is retained in installed records.
Identifiers start with an ASCII letter or digit and contain only ASCII letters,
digits, `.`, `_` and `-`; ids/targets allow 64 bytes and plugin names 256 bytes.
Relative artifact/watch paths allow 4096 UTF-8 bytes and 32 normal components.
Build metadata allows 1–64 argv elements, 4096 bytes per element, 16 KiB total,
at most 256 watch paths and a 1–600 second deadline. NUL is rejected in paths
and arguments. Portable keys use the Agent catalog's key byte bound and reject
duplicates and control characters.

Installation and enablement are separate state changes. Enabling selects the
exact installed record for the current target; reinstalling an id leaves its
enabled record unchanged until another enable operation. Disabling stops future
selection; uninstall requires the id to be disabled. These operations publish
source intent, not a Runtime apply result. They retain content-addressed source
objects, and never prune a live Loader's cache. Runtime loading separately checks
the recorded SHA through `NativeCatalog::load_exact` before code execution.

The store owns at most 128 installed ids, 256 source objects and 2 GiB of source
object bytes; constructor limits can tighten object count/bytes. Each artifact
uses the Loader's artifact bound. A manifest is at most 64 KiB, the atomic state
index at most 1 MiB, and explicit Portable keys at most 64 per manifest. Index
reads validate format, identities, duplicate ids, cardinalities and selected
records before use. Writers hold an independent cooperative directory lock;
all index/object operations use pinned directories and no-follow regular files.
Store directories and index/object files must belong to the effective user and
deny group/other writes. Source manifests and build inputs retain their explicit
caller-selected trust.
Private staging is bounded and removed on ordinary failure. An immutable object
may remain if later index publication fails. The consumed source copy determines
its digest even when the public build output changes during copying.
Under the writer lock, the store reclaims abandoned regular, user-owned staging
files in its reserved `.rsi-addon-<32 lowercase hex digits>.tmp` namespace,
subject to the index or artifact byte bound. Writers reject unmanaged root
entries; installation also rejects unmanaged object entries. Snapshot reads validate the index rather
than hashing all retained objects; installation checks object storage quotas and
verifies any reused digest object, and activation verifies the selected bytes.

A source-state publication uses file sync and atomic rename. Its receipt reports
whether the containing directory sync succeeded after publication; that later
failure never becomes an ordinary pre-publication error or triggers a rollback.
Root and object-directory replacement reject subsequent operations. These are
local storage mechanics, without a Windows writer, remote marketplace or version
solver.
