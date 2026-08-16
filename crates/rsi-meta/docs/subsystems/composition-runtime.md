# Composition runtime

## Projects, workspaces, and graph resolution

A project is an offline candidate manifest plus an optional lock path. Validation returns diagnostics for invalid candidate content, schema, lock, or dependency graphs; environmental and file-I/O failures are errors. Locking is create-or-verify: a missing lock is atomically created, equivalent normalized content is unchanged, and different existing content is `lock_conflict`.

A workspace fixes the database, immutable cache, installed manifest, and installed lock. `open` and `install_offline` take the same database-derived exclusive lease. On Unix the lease combines a path sidecar with an owner-only physical-identity guard keyed by the database device and inode; it never locks the SQLite file itself, so hard-link aliases remain exclusive without interfering with SQLite's native locks. Two absent installed files mean an empty revision-zero graph; two present files are recovered and loaded; one without the other is `torn_installed_pair` unless the durable pair journal can finish or restore the transaction.

Scopes form a rooted ownership tree. Required and optional service injections form dependency edges. Resolution starts in the consumer scope and walks ancestors; the first scope containing providers wins, siblings are not searched, and an explicit binding is required to cross branches. An explicit binding is authoritative even for an optional injection: an unavailable selected provider does not fall back to another provider. An unresolved required injection remains inactive without instantiating plugin code.

## Apply and generation lifecycle

The registry is the sole graph writer. Reads and admitted streams use one immutable routing snapshot while a candidate is validated, staged, and prepared. Prepare is fallible and invisible; failure aborts prepared generations in reverse order. Commit installs the pair with the lock last, commits active state/result/event/revision in SQLite, and atomically publishes routing. A post-persistence publication failure makes the host terminal so recovery, rather than continued service, reconciles state.

```text
descriptor -> preparing -> prepared -> committed -> retiring -> retired
                  |   |                     |
                  |   +--> prepare_failed   +-- leases remain --> retiring
                  +------> aborted
```

Commit notifications cannot veto publication. `Prepared` and `Retired` acknowledgements each have a 30-second hard deadline; timeout aborts a candidate or force-stops a retiring runtime instead of wedging the registry. Each admitted call or stream pins a generation; replacement neither moves nor terminates it. Retirement runs in reverse dependency order after leases drain. The loader keeps native libraries mapped until process exit even after instances retire.

`GraphRevision` advances only for a graph that becomes active. `HostSnapshot` contains that graph, its event cursor, token generation, and current composition digest; desired-state and restart intent are not observable snapshot state.

## Durable operations and recovery

Apply, offline install, token rotation, and shutdown have caller-owned `OperationId` values. External IDs cannot use host-reserved internal prefixes. An ID binds the method, absolute lexically normalized project paths, and explicit parameters—not later file contents. The first execution persists one candidate snapshot. A matching retry returns the same terminal result even after source files change or disappear; a different request returns `operation_id_conflict`.

Only those side-effecting operations have durable results. Read IDs are transport correlations. Deterministic business rejections may be replayed. A live infrastructure failure before reservation or an external effect leaves the ID reusable; if the process crashes after durable reservation, recovery seals an honest terminal `*_not_committed` result so the same ID cannot ambiguously start different work. v5 startup first reconciles older pending apply state and retains successful legacy side-effect IDs as non-reusable tombstones; old read outcomes are discarded.

The pair journal records old and candidate bytes around manifest/lock replacement. The lock is the filesystem commit marker. Recovery restores a mixed pre-marker pair or finishes a completely installed pair and terminal operation. Offline install commits files and its result but no graph revision or event; the next successful `open` activates the digest once.

## Process-fixed and plugin-origin changes

Online apply first performs process-fixed preflight. When the candidate crosses a process-fixed boundary it persists and returns `RestartRequired`, without staging or mapping the candidate, changing installed files, emitting a graph event, incrementing revision, or stopping the process. The external caller explicitly shuts down, waits, installs offline, and opens a fresh host.

A plugin-origin apply cannot coordinate that process boundary and receives `process_fixed_requires_external_install`. The process records mapped process-fixed artifact fingerprints; attempting to replace them inside that process is `fresh_process_required`.

Development file watchers may request hot apply only from the current admitting generation, with `control.apply-manifest`, for the fixed installed pair. Rejection audit identity includes the source generation and graph revision. Effect identity instead binds the source composition and instance, the plugin command ID, and the registry-rebuilt candidate lock content, so reusing a plugin-local ID for changed bytes cannot replay an older apply. These reserved identities are distinct from caller-owned `OperationId` values. Plugins never write the installed lock themselves. Non-process-fixed drift follows the same prepare, journal, durable commit, cutover, and retirement path as an external apply.

The runtime returns `applied`, `unchanged`, `restart_required`, `rejected`, or transient `failed` feedback to the originating plugin on the bounded control lane. Queue saturation is reported as transient failure rather than faulting the generation. The HMR consumer keeps at most one apply in flight, retries transient failure, and stops retrying the same content after a deterministic rejection or external-restart requirement.
