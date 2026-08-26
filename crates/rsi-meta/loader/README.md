# rsi-meta-loader

`rsi-meta-loader` is both the trusted native execution adapter and an ordinary
plugin. It preserves core's `Runtime -> Context -> Fiber` model; catalog,
mapping, callback, and foreign serialization policy remain outside core.

## Catalog and artifact identity

`NativeCatalog` exclusively owns a dedicated cache directory plus load,
callback, live-instance, finalizer, destruction, staging-byte, and durable-byte
admission. The source path is caller-selected authority, not an allowlist.
Artifacts are trusted process code.

The catalog rejects symlinks, special files, unmanaged cache entries, zero or
overflowing limits, and callback deadlines above 24 hours. It first hashes a
bounded regular source through a fixed 64 KiB buffer. A live digest reuses its
verified mapped module. A miss performs a second bounded read into a private
stable staging file; that copy's digest is authoritative if the source changed
between reads.

One digest gate admits the cold owner before staging. Waiters recheck source
identity after the gate and rekey when bytes changed rather than sharing the
wrong winner. Durable and live-staging budgets are independent. A cold artifact
passes ABI entry validation before cache commit. Commit recomputes the stable
digest while copying, syncs the private copy, publishes without replacement,
compares the durable name against staged bytes, and only then commits ledger
capacity.

On Unix, the catalog pins and locks the directory object; enumeration,
temporary creation, digest open, publication, comparison, rollback, and
durability sync resolve relative to it. Removing the cooperative lock marker
does not admit a second owner. Public-path replacement poisons later admission
but is never cache I/O authority. A failed final durability fence removes and
syncs only the claimed directory's entry. Unprovable rollback poisons the
catalog.

On Windows, the marker handle denies delete sharing. Private staging begins
with an exclusive writer, then reopens read-only with write/delete sharing
denied and rechecks length and digest before mapping. The retained handle pins
those bytes through unload. Windows behavior is claimed only when the real
native suite runs on Windows.

The loader maps the verified private artifact rather than reopening a durable
pathname. Live workers, factories, instances, issued capabilities, and
finalizers retain the catalog lease and mapping. Operator cache cleanup requires
that no catalog owns the directory.

## ABI v2 adoption

The catalog resolves only `rsi_meta_plugin_entry_v2`. A v1-only artifact is
rejected. It validates entry status, version direction, minimum table prefix,
mandatory exchange pointer, factory identity, and prepared output before
returning `NativeFactory`. An explicit non-OK entry remains an entry failure;
an OK status with an invalid table is ABI incompatibility.

Every raw output is adopted with its issuer-owned release token before status
interpretation. Malformed output, partial entry, failed prepare, and partial
create release or destroy all transferred ownership exactly once. A capability
ID never points to a stack-local bridge object.

`NativeFactory` implements core's identity, per-attempt prepare, and activate
seams. Prepare serializes the unchanged desired configuration, receives one
normalized configuration plus exact requirements and a declared retained-byte
charge, retains the opaque prepared capability before releasing its output,
and stores it in one single-use `PreparedActivation`. It has no Context and no
injection authority. Activation consumes that prepared capability exactly once,
creates a native instance, registers instance lifetime cleanup before entering
plugin setup, imports exact injected service capabilities, and gives native
code callback-local dynamic provide and effect authorities. The Loader never
auto-publishes services from static descriptor metadata.

One native service callback receives one host-owned callback frame and one
callback-lifetime provider-oriented `ProviderChannel` capability for the
complete bidirectional call. It receives requests, sends responses, and observes
cancellation; it cannot finish caller requests or consume caller terminal
state. Native `CAP_OPEN` returns the distinct caller-oriented `CallChannel`,
which sends and one-shot finishes requests, receives responses through EOF, and
then observes exactly one cached terminal outcome. The v1 behavior of starting
a new foreign callback for each byte frame does not exist.

Native outbound use operates transferred core capabilities directly. Message
bytes and capability arrays are validated and admitted together before foreign
pointers are borrowed. Every `CAP_OPEN` names the exact live callback-local
scope capability that owns the borrowed result. Same-instance same-lineage
reentry fails before gate wait for every port; unrelated lineage contention is
`BUSY`. No Runtime, catalog, capability-table, or cache lock crosses the foreign
exchange.

The stable host table owns bounded slot-plus-epoch capability and output tables
without retaining the module-control object whose finalization waits for their
drain. Callback-local activation, effect, provider, and call-channel entries are
non-retainable and are sealed with their exact callback frame. Transferable
service entries and output-owned initial leases remain until their explicit
one-shot release. Each table keeps a free-index frontier, so ordinary reserve,
release, and rollback are amortized constant work while epoch exhaustion retires
only the exhausted slot. A repeated release of the still-current consumed epoch
is a protocol error; after reuse advances the epoch, the old token is stale.
Persistent service slots store core `DetachedCapability`
values rather than a strong holder Context; a use temporarily reconstructs only
the original holder while its Runtime still exists. The entry stays charged,
but the table cannot form `Runtime -> module -> HostTable -> Runtime`.
Every successfully moved plugin cleanup capability is held by a drop-safe,
single-owner Loader lease. Invoking its core cleanup or dropping that cleanup
without invocation schedules the same ordered `RUN_CLEANUP` then `CAP_RELEASE`
job before module finalization. A rejected move schedules neither operation and
leaves the lease plugin-owned, including when rejection races core retirement.
Snapshot counters expose current, peak, and rejected host
capability slots, output slots, and output bytes.

Module finalization first closes ordinary raw admission and drains exchanges,
then destroys the factory and invokes `FINALIZE` through one exclusive lane. A
library that was mapped but rejected before a compatible plugin table existed
is also released on its reserved module FIFO rather than on the loading thread.
This keeps loader-lock-sensitive library destructors off caller and callback
stacks. A
failed finalization output token is released on that lane before ordinary
admission reopens. Successful `FINALIZE` invalidates the raw table immediately;
no guard, pointer, or destructor touches it afterward. Only successful factory
destruction plus a fully validated successful `FINALIZE` permits unmapping.
Refusal, panic, or malformed success records a retained finalization and pins
the complete bundle rather than risking use-after-free. That fail-closed bundle
also retains its catalog lease, staged artifact accounting, and cooperative
catalog lock. Recording the first such failure permanently closes new load
admission for that catalog; work admitted earlier remains bounded by the
catalog's existing load and artifact limits and may still finish or retain its
own failed bundle. Operator cache cleanup remains blocked until process
recovery.

## Workers, timeout, and destruction

Foreign callbacks run on dedicated OS threads, not Tokio's blocking pool. The
catalog acquires callback admission before thread creation. Each factory or
instance gate linearizes idle, busy, and poisoned in one admission state;
callers cannot observe an old poison fact and later claim a reopened gate.
These gates are fail-fast and do not retain hidden waiter queues.
Destruction and factory finalization have separately reserved bounded lanes, so
ordinary callback saturation cannot strand teardown.
Loader-owned panic payloads and payloads raised while destroying them cross
separate unwind boundaries. Only a final payload whose own destruction panics
again is deliberately forgotten so teardown can continue.

Each callback has one atomic completion-or-timeout decision. Activity includes
foreign execution, result delivery, failed-delivery result destruction, and
worker-closure exit. Completion is published before the gate becomes reusable.
A timeout winner publishes poison before the gate can become observable as
reusable; a callback return alone cannot reopen admission before result handoff
and timeout arbitration finish. A create or call timeout poisons the adapter
and terminalizes the Runtime before foreign completion can win.

Timeout is not termination. The timed-out thread, callback frame, mapped
library, capability table entries, effect transaction, instance gate, cache
lease, and logical resources remain owned until foreign code actually returns.
Later admission is fenced immediately. Pre-application preparation has no
Runtime authority and poisons only its factory; catalog entry timeout fences
its digest and cache lease.

Callback and destruction capacity belongs to the complete `NativeCatalog`.
Sharing one catalog across Runtimes therefore deliberately shares the damage
from callbacks that never return. Likewise, trusted foreign destruction cannot
be safely preempted: if every reserved destruction worker is stuck, later
module queues remain bounded but cannot make progress. Giving every module an
escape worker would violate the catalog's thread limit; recovery is a
process-level concern for code that does not honor the trusted native contract.

Before entry, the catalog reserves a factory-finalizer slot. Before create, it
reserves a live-instance slot retained through actual destruction. Publication
failure or direct last-owner drop transfers the instance and its reservation to
a physically bounded reserved destruction queue. Runtime cleanup joins
registered instances. Factory destruction follows the last external
`NativeFactory`, instance, or issued-capability owner; Runtime shutdown cannot
claim ownership it does not hold.

`NativeCatalog::snapshot` reports logical cache, staging, load, callback,
instance, finalizer, destruction, and capability use with high-water and
rejection counts. It does not report allocator RSS.

## Loader plugin service

`LoaderFactory` is applied like any other plugin and dynamically provides the
`rsi.meta.loader.v1` service. That string versions the Loader's JSON command
protocol and is independent of native ABI v2.

Initial configuration accepts at most 1,024 unique bounded IDs. Mapping is
bounded by the smaller of eight and catalog load concurrency. Entries sharing a
factory prepare in configuration order under its fail-fast gate; distinct
factories prepare up to Runtime preparation admission. No child Fiber becomes
visible before its own bounded preparation succeeds.

Commands load, reconfigure, unload, and inspect named child Fibers. IDs are
slot-and-epoch claims retained through mapping, child application, response
delivery, rollback, unload, and cleanup. A stale or cancelled task cannot
publish or remove a later reuse. Dynamic load acquires catalog admission before
claiming an ID or spawning persistent work.

Command payloads and responses use core Messages. JSON bytes obey the Runtime
frame limit and command responses carry no hidden native handles. Oversized
success is not acknowledged: the owned task rolls back its child and returns a
fixed bounded error when possible. Inspect clones the bounded handle map under
its mutex, then snapshots and serializes after unlocking.

A child with missing actual supplies is validly Pending. Loader does not create
a global graph transaction or a second dependency authority.
