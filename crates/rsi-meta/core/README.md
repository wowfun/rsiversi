# rsi-meta

`rsi-meta` exposes two public façades. `CompositionProject` validates or locks an offline candidate without opening durable host state. `CompositionHost` is the only online embedded interface: it owns one workspace lease, serializes graph mutation, publishes immutable routing snapshots, persists domain operations and events, and retires replaced generations.

## Projects and workspaces

`CompositionProject` names a candidate manifest and optional lock. `validate` reports candidate, schema, lock, and dependency-graph diagnostics; filesystem and environment failures remain `HostError`. `lock` atomically creates a missing lock, returns `Unchanged` for equivalent normalized content, and rejects different existing content as `lock_conflict`.

`CompositionWorkspace` names the database, cache, and installed manifest/lock pair. `CompositionHost::open(OpenOptions)` claims a non-blocking workspace lease until termination. An absent installed pair is revision-zero empty state; a complete pair is recovered, validated, loaded, and activated; a lone file is `torn_installed_pair` unless an existing transaction journal can recover it.

## Operations and lifecycle

Call `apply(ApplyRequest)` for online changes. `OperationId` binds the method, normalized project paths, and parameters to the first durable candidate snapshot, so a retry can succeed after source files change or disappear. Results are `Applied`, `Unchanged`, or `RestartRequired`; business rejection uses `HostError::OperationRejected`, while I/O and storage failures keep their infrastructure types.

A process-fixed result is preflight only: it does not install files, stage or map a library, emit a graph event, increment the revision, or stop the host. The caller requests shutdown, waits for termination, calls `CompositionHost::install_offline`, and opens a fresh host. Offline install commits files and its idempotent result; the subsequent `open` performs the one graph activation.

`snapshot`, `events_after`, `subscribe`, `inspect_plugin`, `open_service`, `rotate_token`, `request_shutdown`, and `wait_terminated` expose the remaining online behavior. Service streams retain their sequence, credit, terminal-frame, and generation-lease protocol. Registry, routing, persistence, runtime actors, recovery, and loader details remain crate-private.

The crate contains no unsafe code. Native ABI and artifact loading are delegated to the plugin and loader packages. Cross-package semantics belong to the [composition runtime](../docs/subsystems/composition-runtime.md) and [protocol reference](../docs/subsystems/protocols.md).
