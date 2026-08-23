# rsi-meta-loader

`rsi-meta-loader` is both a native execution adapter and a plugin.

`NativeCatalog` bounds an artifact at 256 MiB, hashes its exact bytes, and
publishes those bytes under the SHA-256 name. Symlinks, special files, and
different bytes already occupying that name are rejected before mapping. On
Unix, the loader maps a private unlinked copy made from the verified bytes, so
later replacement or in-place mutation of the durable cache path cannot change
the staged content. The private descriptor remains writable by trusted code in
the host process; native execution is not a process-isolation boundary. The
Windows implementation retains the verified cache handle
with restrictive sharing; its native behavior requires Windows evidence. The
catalog then validates the v1 ABI
table, reads the bounded descriptor, and returns a `NativeFactory`. Live
factories and instances retain both the mapping and artifact handle. The
identity used by core is the verified SHA-256 digest, never an untrusted path or
self-reported revision.
An explicit non-OK plugin entry status is reported as an entry failure with the
exact status; ABI incompatibility is reserved for an entry that returned OK
but published an invalid table. Either path destroys a partially published
factory exactly once when the paired destructor is available.
Concurrent loads of one digest share a catalog gate. A live digest reuses its
already verified mapped module without consulting the durable cache, creating
another staging copy, or invoking the native entry again. A cold digest
verifies the durable cache identity before mapping it. If loading, entry, or
descriptor discovery exceeds its deadline, the digest gate rejects re-entry
while the abandoned worker is still inside native code; the fence expires only
after that worker actually returns.

Plugin-returned buffers are structurally checked before the Loader reads them.
An allocator-matched release guard owns each buffer until copying or rejection
finishes, so malformed metadata and unwinding cannot skip its one release.

`NativeFactory` adapts configuration, activation, service frames, outbound
required-service calls, and destruction to core's `PluginFactory`. Foreign
callbacks run on dedicated operating-system threads rather than Tokio's shared
blocking pool. Factory callbacks share one adapter-owned gate; every instance has its
own gate that remains held until a detached foreign call actually exits. Each
callback has one deadline and one atomic completion-or-timeout decision. The
completion marker is published before that gate is released, so success and
terminalizing timeout cannot both win. A create or service callback timeout
poisons the gate and terminalizes the Runtime, fencing queued and new work.
Pre-application config validation has no Runtime
authority; its timeout poisons only the factory and returns an error. Worker
disconnects are reported as callback failures rather than timeouts. Instance destruction is offloaded and reported through cleanup;
factory destruction is offloaded with the mapping retained. Fallback instance
destruction is also offloaded when activation cannot register its cleanup. A
failed create destroys any non-null partial instance transferred by the ABI.
Outbound service calls requested synchronously by native code wait through the
current runtime handle on the dedicated foreign worker. The async native
endpoint yields while that worker waits, leaving provider tasks free to run;
the bridge therefore supports both current-thread and multi-thread Tokio
runtimes.

`LoaderFactory` is applied like any other plugin and provides the exact `rsi.meta.loader.v1` service. Its initial config accepts at most 1024 entries with unique IDs of at most 128 bytes, then maps and converts them with at most eight concurrent preflight operations into opaque core-prepared applications before the first child Fiber is applied, so a normalizer runs exactly once and a failed preflight publishes no child. Prepared children are then applied in configuration order. Service commands load, reconfigure, unload, or inspect named child Fibers. A missing dependency leaves a loaded child validly `Pending`; the Loader does not perform a global graph transaction.
Loader IDs are adapter-owned atomic reservations. Concurrent or cancelled load
commands cannot publish the same ID twice or strand an in-flight claim. A
runtime-owned load retains its reservation until the response is delivered; if
the caller disappears first, it retains the reservation through child rollback.
An ID likewise remains reserved until its unload cleanup finishes.
