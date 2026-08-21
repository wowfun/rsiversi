# Composition runtime

## Projects, workspaces, and graph resolution

A project is an offline candidate manifest plus an optional lock path. Validation returns diagnostics for invalid candidate content, schema, lock, or dependency graphs; environmental and file-I/O failures are errors. Locking is create-or-verify: a missing lock is atomically created, equivalent normalized content is unchanged, and different existing content is `lock_conflict`.

A workspace fixes the database, immutable cache, installed manifest, and installed lock. `open` and `install_offline` take the same database-derived exclusive lease. On Unix the lease combines a path sidecar with an owner-only physical-identity guard keyed by the database device and inode; it never locks the SQLite file itself, so hard-link aliases remain exclusive without interfering with SQLite's native locks. After SQLite opens the path, the host rechecks that physical identity before applying pragmas, creating the schema, or running maintenance. Two absent installed files mean an empty revision-zero graph; two present files are recovered and loaded; one without the other is `torn_installed_pair` unless the durable pair journal can finish or restore the transaction.

Scopes form a rooted ownership tree. Required and optional service injections form dependency edges. Resolution starts in the consumer scope and walks ancestors; the first scope containing providers wins, siblings are not searched, and an explicit binding is required to cross branches. An explicit binding is authoritative even for an optional injection: an unavailable selected provider does not fall back to another provider. An unresolved required injection remains inactive without instantiating plugin code.

Manifest validation rejects more than 1,024 scopes, scope depth 64, 1,024 instances, or 16,384 bindings before package preparation. The 65,536 service-requirement bound is enforced during graph resolution, after package descriptors supply their injected contracts and before any runtime is launched. Scope validation is linear, providers and ancestors are indexed once, and inactivity propagates only through a reverse-dependency work queue rather than repeated full-graph scans.

## Apply and generation lifecycle

The registry is the sole graph writer. Mutation commands remain on one serialized lane; replay, subscription-boundary capture, and inspection use a separate bounded query lane. One blocking preflight reads, hashes, normalizes, and validates the candidate without mutating the cache. After process-fixed checks, a second blocking pass stages immutable CAS artifacts and rechecks the preflight identity. Any staging failure abandons the reservation so the same operation identity remains retryable; a successfully staged but changed identity is a durable rejection. Native preparation pumps queries, host-state calls, and runtime faults while that snapshot remains invisible. Prepare failure aborts generations in reverse order. The brief commit authority installs the snapshot bytes with the lock last, commits active state/result/event/revision in SQLite, then publishes the new admitting snapshot before stopping admission on replaced generations. A post-persistence publication failure makes the host terminal so recovery, rather than continued service, reconciles state.

```text
descriptor -> preparing -> prepared -> committed -> retiring -> retired
                  |   |                     |
                  |   +--> prepare_failed   +-- leases remain --> retiring
                  +------> aborted
```

Every lifecycle operation has one 30-second deadline covering bounded-lane admission, callback execution, and acknowledgement. Plugin callbacks run on one dedicated current-thread executor per generation, outside general Tokio workers. A pre-commit timeout rejects the candidate. A failed `Committed` acknowledgement occurs after durable state can no longer be rolled back, so the host stops all admission and terminates its registry; a fresh process must recover before serving again.

Each admitted call or stream pins a generation; replacement neither moves nor terminates it. Retirement runs in reverse dependency order after leases drain. A timeout stops admission and bounds host-side waiting, but safe Rust cannot kill an arbitrary stuck native callback or reclaim its thread. The loader therefore keeps native libraries and any unjoined callback thread mapped until process exit. Deployments that require hard per-plugin termination need a subprocess boundary and accept its IPC cost.

A malformed frame that destroys runtime-level protocol synchronization marks the generation faulted, stops new admission, persists `runtime_faulted`, and exposes `Faulted` in graph snapshots. A violation confined to one identified stream emits exactly one valid host-owned cancel and leaves other streams available. Reapplying the same manifest and lock replaces an unhealthy generation instead of returning `Unchanged`.

`GraphRevision` advances only for a graph that becomes active. `HostSnapshot` contains that graph, its event cursor, token generation, and current composition digest; desired-state and restart intent are not observable snapshot state.

## Durable operations and recovery

Apply, offline install, token rotation, and shutdown have caller-owned `OperationId` values. External IDs cannot use host-reserved internal prefixes. An ID binds the method, absolute lexically normalized project paths, and explicit parameters—not later file contents. The first execution persists one candidate snapshot. A matching retry returns the same terminal result even after source files change or disappear; a different request returns `operation_id_conflict`.

`Applied` and `Unchanged` expose one current atomic host snapshot rather than storing a graph copy in every durable operation result. A retry or a concurrent later cutover can therefore return a snapshot newer than the operation's original revision; the durable event stream owns historical operation-to-revision correlation.

Only those side-effecting operations have durable results. Read IDs are transport correlations. Deterministic business rejections may be replayed. A live infrastructure failure before reservation or an external effect leaves the ID reusable; if the process crashes after durable reservation, recovery seals an honest terminal `*_not_committed` result so the same ID cannot ambiguously start different work. Full operation results are retained for seven days and at most 100,000 terminal rows; compaction then keeps only the request identity and terminal classification, so a retry returns `operation_expired` rather than becoming a new operation.

Events use the same seven-day and 100,000-row retention limits and publish a durable minimum available cursor. State is bounded before mutation to 4,096 keys, 4,096 tombstones, and 16 MiB per instance, plus 16,384 keys and 64 MiB per composition. The database has a 512 MiB page limit, WAL autocheckpointing, periodic passive checkpoints, and incremental vacuum. The pre-release store accepts only the schema version declared by its source schema and rejects older or newer databases without mutation; no migration machinery or legacy outcome shape is retained.

The pair journal records old and candidate bytes around manifest/lock replacement. The lock is the filesystem commit marker. Recovery restores a mixed pre-marker pair or finishes a completely installed pair and terminal operation. Offline install commits files and its result but no graph revision or event; the next successful `open` activates the digest once.

## Process-fixed and plugin-origin changes

Online apply first performs process-fixed preflight. When the candidate crosses a process-fixed boundary it persists and returns `RestartRequired`, without staging or mapping the candidate, changing installed files, emitting a graph event, incrementing revision, or stopping the process. The external caller explicitly shuts down, waits, installs offline, and opens a fresh host.

A plugin-origin apply cannot coordinate that process boundary and receives `process_fixed_requires_external_install`. The process records mapped process-fixed artifact fingerprints; attempting to replace them inside that process is `fresh_process_required`.

Development file watchers may request hot apply only from the current admitting generation, with `control.apply-manifest`, for the fixed installed pair. Rejection audit identity includes the source generation and graph revision. Effect identity instead binds the source composition and instance, the plugin command ID, and the registry-rebuilt candidate lock content, so reusing a plugin-local ID for changed bytes cannot replay an older apply. These reserved identities are distinct from caller-owned `OperationId` values. Plugins never write the installed lock themselves. Non-process-fixed drift follows the same prepare, journal, durable commit, cutover, and retirement path as an external apply.

The runtime returns `applied`, `unchanged`, `restart_required`, `rejected`, or transient `failed` feedback to the originating plugin on the bounded control lane. Queue saturation is reported as transient failure rather than faulting the generation. The HMR consumer keeps at most one apply in flight, retries transient failure, and stops retrying the same content after a deterministic rejection or external-restart requirement.
