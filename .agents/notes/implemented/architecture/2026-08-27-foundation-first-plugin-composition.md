---
name: Foundation-first static plugin composition
comment: Correct the ownership seams between Meta, product plugins, and the standard host
---

## Problem

In-process safe-Rust collaborators and cross-ABI capabilities need different
invocation semantics without creating different lifecycle owners. Treating
both as serialized capability calls made local composition shallow and costly,
while family adapters such as Agent Meta and AI Meta coupled implementations
whose dependencies and generations were independent. Profile parsing,
application catalogs, and bootstrap policy also needed owners without growing
Meta into a product registry or introducing a second runtime.

## Decision

`Runtime -> Context -> Fiber` is the sole lifecycle graph. Meta exposes two
deliberately separate contract lanes:

- Local contracts use Rust type identity and direct effect-owned safe-Rust
  objects. Exact hard requirements participate in dependency convergence;
  point-of-use lookup does not add an edge. Local events fix their dispatch
  mode in the marker and are visible only by exact type and isolation.
- Portable contracts retain bounded messages, capability transfer,
  generation fencing, deadlines, cancellation, and the native ABI. There is
  no automatic Local/Portable conversion or optional Portable discovery.

Inherited immutable `ContextExtension` metadata was removed, not migrated to
Local contracts. Product-specific immutable metadata belongs in an explicit
typed wrapper or its owning module; an effect-owned Active-gated Local service
is not a substitute for inherited Context metadata.

Resolvers construct immutable `ResolvedFactory` values before plugin code
runs. Factory identity, revision, provenance, and update mode therefore belong
to composition, not to executable factory implementations.

`rsi-meta-profile` owns one ordered, bounded Profile program, pure frozen Rhai
evaluation, strict patches and includes, all-before-mutation source,
expression, identity, resolution, and watch-plan preflight, capacity-aware
just-in-time leaf preparation during replay convergence, rollback, degraded
recovery, restart-required status, and typed `ProfileControl`. Its bootstrap is
an ordinary Meta plugin whose leaves are child Fibers. It does not own a runtime
or catalog.

`rsi-host` is the generic static composition SDK. It freezes one explicit
single-version linked catalog, Local marker mappings, Host paths, Profile
environment, linked fragments, and launch patches, then directly constructs
exactly one Profile bootstrap. It contains no product implementation, package
resolver, mutable root Context, or second lifecycle engine.

Runtime-composed providers, routers, stores, registries, schedulers, policies,
the Agent Kernel, and the Agent executor export independent ordinary plugin
factories with exact Local requirements. Protocols, DTOs, transports, context
projection, helpers, and test kits remain libraries. Family-level `rsi-ai-meta`
and `rsi-agent-meta` adapters and the broad Agent protocol are absent.

The standard `rsi` product owns Base and Headless factory registration and
Profile fragments. Its Headless runner uses the durable Agent seams. Image
outputs are committed individually through Media and persisted only as refs;
tail failure preserves the durable prefix. Effect-owned pre-terminal
finalizers settle process-local Jobs before the sole terminal Fact.

Approval, Permission Presets, and Sandbox are independent Local services, not
ambient enforcement. The current Tool registry contains trusted process-local
definitions and no effect taxonomy or standard effect-bearing registration.
Before the standard product admits its first effect-bearing Tool, that Tool's
owning plugin and process-spawning `ToolExecutor` must consume explicit policy,
approval when required, and a Sandbox plan at the actual effect site, then
place the decision and truthful enforcement stamp at the Agent durability
boundary. Merely registering these services does not claim that enforcement
already exists.

Factory construction may impose a product-owned readiness invariant that
cannot be weakened by Profile replacement. The standard composition constructs
the local Sandbox factory in required mode whenever it links coding Tools; the
factory must select a behaviorally verified restricted backend before it
publishes Sandbox. Generic optional composition retains the explicit
unconfined-only case. Readiness remains a provider activation property inside
the sole Meta lifecycle graph rather than a second global readiness graph.

Native support is split into `rsi-meta-native` and
`rsi-meta-native-loader`. ABI v3 accepts only explicit trusted artifact paths,
supplies exact Portable contracts, and has no Loader service, package
installation, version solving, compatibility entry point, or artifact watcher.

## Alternatives considered

Keeping every service Portable would preserve one call mechanism, but would
force direct safe-Rust objects through serialization and confuse possession of
an `Arc` with revocable cross-boundary authority.

Copying DeepSeek Harness services into a separate in-process host would make
local calls direct, but would bypass Meta generation ownership, cleanup, and
dependency replay. The useful distinction between runtime plugin,
protocol/transport library, and application composition is retained without
copying its lifecycle model.

Keeping family-level AI and Agent adapters would reduce migration work, but
would preserve the ownership defect by forcing independent implementations
into one deployment generation.

A dynamic package catalog and multi-version resolver were rejected because
linked Rust code is already selected by Cargo. Native code remains an explicit
trusted-path escape hatch rather than an installation system.

An atomic shadow Runtime for Profile reload was rejected because it creates a
second graph and cannot atomically transfer arbitrary external effects. Replay
and explicit observed/target state report the actual convergence semantics.

## Consequences

Local discovery and managed dependencies are generation-owned, but an `Arc`,
Future, or Stream already handed to trusted safe-Rust code can outlive provider
retirement. Forging universal revocation would collapse Local back into the
Portable call kernel.

Profile reload can temporarily withdraw services because Meta retires an old
generation before activating its replacement. Candidate failure reconstructs
the previous target; failed compensation publishes a retryable degraded graph
instead of claiming atomicity or zero downtime.

The pre-release cutover intentionally provides no ABI, Profile, or durable
format compatibility facade. Every in-tree consumer, fixture, schema, test,
documentation owner, and CI failure domain moves together.

Headless live output may lead the durable prefix after a process crash;
per-Fact JSONL watermarks expose that fact, while the terminal outcome is not
published until its prefix is durable. Safe Rust cannot force-stop a Job that
ignores cooperative cancellation, so bounded finalization timeout becomes the
turn's sole durable failure rather than a false successful terminal.

Current conformance exercises Linux native loading and ABI behavior. Windows
and macOS native claims require their respective CI runners and are not
inferred from local Linux results.
