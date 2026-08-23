# rsi-meta architecture

The core is one deep module with three public concepts:

```text
Runtime
  └─ persistent COW Context (owner + isolation + direct-edge intercept + trace)
       └─ apply(factory, config)
            └─ Fiber generation
                 ├─ dependency bindings
                 ├─ provided services and event listeners
                 ├─ child Fibers
                 └─ reverse-ordered effects
```

Every apply creates a Fiber. A Fiber resolves all requirements from one registry snapshot, remains `Pending` when the snapshot cannot satisfy them, and activates only after a matching publication makes convergence possible. A declaration index is updated atomically with Fiber ownership and lets cycle diagnosis traverse only provider declarations reachable from the pending Fiber. Service-change convergence runs independent Fibers with a Runtime-configured concurrency bound; each Fiber still serializes its own transitions. Setup mutations remain staged. Publication makes the generation's services and listeners visible together; setup failure disposes staged ownership without publication.

Retirement reverses ownership. It closes admission and withdraws publications, asks dependent Fibers to converge, waits for their binding leases and admitted callbacks, disposes children, then runs effects last-in-first-out. A service handle contains both provider and caller generation identity, so a value captured from an old activation cannot silently route through a new graph. Runtime-wide service-call admission bounds the number of live calls independently of each call's bounded channels.

Provider admission closure and its live-callback count share one atomic state.
A concurrent ordinary callback therefore either increments the count before
closure or observes the closed gate. Cleanup-time calls from an already
retiring dependent remain inside the dependent convergence transaction, which
the provider joins before checking that its callback count drained.

The native path is an adapter chain:

```text
Loader Fiber → NativeCatalog → verified hash + mapped ABI → NativeFactory → child Fiber
```

The Loader has no privileged access to Runtime internals. Its service handler uses its own provider Context to apply children. Native, future process, and future Wasm execution therefore converge at `PluginFactory` rather than adding branches inside core.

A timed-out in-process native create or call terminalizes the complete Runtime,
not only one module. This is an intentional blast radius: trusted native code
shares memory and may already have corrupted global invariants, so a
module-local fence would claim isolation that does not exist. Process or Wasm
adapters are the route to smaller trustworthy failure domains.
