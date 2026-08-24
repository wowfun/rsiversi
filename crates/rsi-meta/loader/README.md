# rsi-meta-loader

`rsi-meta-loader` is both a native execution adapter and a plugin.

`NativeCatalog` owns native admission and a dedicated cache directory. Every
staged load bundles its private artifact ahead of its catalog lease in teardown
order, so success, rejection, worker disconnect, and post-timeout worker exit
all remove owned staging before the last cache lock can be released. On Unix,
it pins and locks the directory object as the primary ownership authority and
also holds the cooperative `.rsi-meta.lock`; unlinking and recreating that marker
cannot admit a second Catalog for the same directory object. All Unix
cache-owned enumeration, temporary creation, digest opening, publication,
comparison, rollback, and durability operations resolve names relative to that
pinned directory. Public-path identity checks only poison a Catalog whose name
was replaced; they are not its I/O authority. Windows instead pins the cache
pathname through the marker's non-delete-shared handle. The catalog bounds an
artifact at 256 MiB and first streams source bytes through a 64 KiB hash buffer
to select its SHA-256 identity. A live identity reuses its
mapped module without a second staging reservation. A miss streams a second
bounded read into a private stable staging file; that copy's digest is
authoritative if the source changed between reads. Cache entries and live
staging bytes have independent aggregate budgets. Symlinks, special files, cache
quota exhaustion, and different bytes already occupying a digest name are
rejected before a load returns. A durable digest is committed atomically only
after the native entry and descriptor have been validated; failed artifacts do
not consume durable cache capacity. The commit copy recomputes SHA-256 while it
streams, so same-length staging mutation cannot publish bytes under a stale
digest. The private commit pathname is checked against its verified open-file
identity before publication, and a newly published durable entry is streamed
against the authoritative staged artifact before accounting. Linux catalogs
retain their pre-landing capacity reservation and mark it committed only after
that comparison and syncing the pinned cache directory. If that final fence
fails, the published name is removed relative to the claimed directory object
and that same directory is synced before the original failure is returned;
pathname replacement therefore cannot make rollback delete replacement-owned
data. If removal or its durability fence cannot be established, the Catalog is
poisoned and rejects
all later loads instead of trusting disk and ledger state that may disagree.
Cache reclamation is an operator action that
requires no catalog to hold the directory lock; the catalog does not perform
implicit eviction.
Only the lock and canonical lowercase digest files may remain at startup;
unmanaged or stale staging entries fail closed and must be removed while no
catalog owns the directory.

The loader maps the private staged copy rather than reopening the durable
cache path, so later cache replacement or mutation cannot change the mapped
content. The private descriptor remains writable by trusted code in the host
process; native execution is not a process-isolation boundary. On Windows,
the staging writer denies sharing. Before mapping, the catalog closes it,
reopens the random path read-only with write/delete sharing denied, and
recomputes its length and digest. The retained read handle then prevents
post-verification replacement or mutation until the module is unloaded. A
Windows cache commit likewise uses an exclusive writer; the verified and synced
private commit copy is published with no-clobber semantics before accounting.
The catalog then validates the v1 ABI
table, reads the bounded descriptor, and returns a `NativeFactory`. Live
load workers, factories, and instances retain the catalog ownership lease as
well as the mapping and artifact handle; the cache cannot be reopened or
operationally cleaned while native code or its teardown still uses staging. The
identity used by core is the verified SHA-256 digest, never an untrusted path or
self-reported revision.
An explicit non-OK plugin entry status is reported as an entry failure with the
exact status; ABI incompatibility is reserved for an entry that returned OK
but published an invalid table. Either path destroys a partially published
factory exactly once when the paired destructor is available.
Concurrent loads of one digest share a catalog gate before private staging. A
live digest reuses its already verified mapped module after the bounded source
hash, without creating a private staging file, consulting the durable cache, or
invoking the native entry again. A cold gate owner alone makes the stable second
copy. A waiter rechecks the source identity after waiting before it shares the
winner; if that check or the stable copy has a different digest, the operation
releases the provisional gate and rekeys the stable artifact under its
authoritative digest. It therefore does not coalesce bytes from a source that
changed between reads. A cold digest compares an existing durable entry once
during the post-validation commit transaction; a new entry is copied once while
recomputing the staged digest, synced, published, and verified once at the
durable name before accounting. If loading, entry, or
descriptor discovery exceeds its deadline, the digest gate rejects re-entry
while the abandoned worker is still inside native code; the fence expires only
after that worker actually returns.

Plugin-returned buffers are structurally checked before the Loader reads them.
An allocator-matched release guard owns each buffer until copying or rejection
finishes, so malformed metadata and unwinding cannot skip its one release.

`NativeFactory` adapts configuration, activation, service frames, outbound
required-service calls, and destruction to core's `PluginFactory`. Foreign
callbacks run on dedicated operating-system threads rather than Tokio's shared
blocking pool. One catalog-owned executor acquires its callback permit before
spawning a thread; exhausted callback admission fails immediately with
`LoaderError::Busy`. A separate catalog-load lease is acquired before staging
and bounds synchronous gate waiters as well as later native callback work.
Factory validation and create callbacks acquire one adapter-owned gate
fail-fast, so a busy factory does not accumulate hidden async waiters. Every
instance has its own gate acquired before its worker is spawned. Destruction
uses a separately bounded worker lane so ordinary callbacks cannot consume its
capacity. Each callback has one deadline and one atomic completion-or-timeout
decision. Callback activity and thread admission remain owned through result
delivery, delivery-failure result destruction, and the end of the OS-thread
closure; a snapshot or new admission therefore cannot observe the callback as
finished while arbitrary result destruction is still running. The completion
marker is published before that gate is released, so success and terminalizing
timeout cannot both win. A create or service callback timeout
poisons the gate and terminalizes the Runtime, fencing queued and new work.
The synchronous ABI is invoked by one admitted OS-thread worker per service
frame. This preserves per-frame deadlines and releases scarce callback/gate
capacity while a streaming caller is idle; it deliberately does not pipeline
frames through one native instance.
Pre-application config validation has no Runtime
authority; its timeout poisons only the factory and returns an error. Worker
disconnects are reported as callback failures rather than timeouts. Instance destruction is offloaded and reported through cleanup;
that cleanup joins the serialized gate and the actual destruction worker rather
than imposing a second adapter deadline. Runtime shutdown therefore owns waiter
timeout while admitted native cleanup continues to quiescence. Before an
artifact is mapped, its module reserves a factory-finalizer slot before native
entry executes. Before create is spawned, the catalog also reserves one live-instance
slot, retained until that instance's destructor actually returns. Registered
Runtime cleanup uses the ordinary destruction lane and joins its instance
callback. Publication failure and direct owner drop transfer the live-instance
reservation and resources to the reserved destruction queue instead. The
queue has one physical slot per possible live instance, so a hung destructor
cannot turn later drops into an unbounded hidden backlog. A full ordinary queue
therefore cannot leak instance or factory resources, staging reservations, or
the cache lease.
A failed create destroys any non-null partial instance transferred by the ABI.
Runtime cleanup completion covers registered instance cleanup invoked by the
Runtime. Module finalization is catalog-owned and follows the last
`NativeFactory`/instance owner, including owners retained outside a Runtime;
the cache lease, not Runtime shutdown, is the completion fence for that final
factory callback.
Outbound service calls requested synchronously by native code wait through the
current runtime handle on the dedicated foreign worker. The async native
endpoint yields while that worker waits, leaving provider tasks free to run;
the bridge therefore supports both current-thread and multi-thread Tokio
runtimes. The host bridge rejects service-identifier and request lengths at
their Runtime limits before borrowing plugin-provided pointers. `CatalogOptions`
rejects zero, overflowing, or over-24-hour callback
deadlines before any native worker can be spawned. `NativeCatalog::snapshot`
reports logical cache and staging use plus load, callback, live-instance, and
destruction activity, high-water marks, and rejected admission; these values
are capacity evidence, not process RSS.

`LoaderFactory` is applied like any other plugin and provides the exact `rsi.meta.loader.v1` service. Its initial config accepts at most 1024 entries with unique IDs of at most 128 bytes, then maps and converts them into opaque core-prepared applications before the first child Fiber is applied, so a normalizer runs exactly once and a failed preflight publishes no child. Initial mapping polls at most the smaller of eight entries and the Catalog's validated concurrent-load limit. Each polled entry obtains shared Catalog load admission before creating its blocking worker, so one batch cannot reject itself merely because its Catalog limit is below eight, while cancelled or concurrent Loader generations still share the same fail-fast authority. After mapping, entries that share one native module normalize in configuration order because that module's factory gate is deliberately fail-fast. Distinct module groups normalize concurrently up to the smaller of the mapping width and the Runtime's validated preparation limit, so one Loader generation does not reject its own valid batch at that fail-fast authority. Prepared children are then applied in configuration order. Service commands load, reconfigure, unload, or inspect named child Fibers. A missing dependency leaves a loaded child validly `Pending`; the Loader does not perform a global graph transaction.
Every command response is serialized directly through the Runtime's service
frame-byte budget. If a complete response would cross that bound, the Loader
discards the partial encoding and returns a fixed small error response; when
even that diagnostic cannot fit, the service returns `PayloadTooLarge`. An
oversized load success is not acknowledged to its owned task, so the child is
rolled back instead of remaining published behind an error response.
Loader IDs are adapter-owned generation-fenced slots. Reservation, publication,
unload retirement, rollback, and release all carry the same opaque claim token;
an older task can neither remove a newer handle nor release its claim. Concurrent
or cancelled load commands cannot publish the same ID twice or strand an
in-flight claim. A runtime-owned load retains its reservation until the response
is delivered; if the caller disappears first, it retains the reservation through
child rollback.
Dynamic load acquires catalog admission before claiming an ID or spawning its
persistent task and blocking worker, and retains that lease through delivery or
rollback. Busy therefore leaves no task or ID history, while the claimed set is
independently capped at the same 1,024-entry topology bound as initial config.
An ID likewise remains reserved until its unload cleanup finishes. Inspect
clones the bounded handle map under the Loader registry mutex, then constructs
Fiber snapshots and serializes the response after releasing that mutex.
