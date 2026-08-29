# rsi-meta security boundary

`rsi-meta` is a bounded composition kernel for safe Rust plus deliberately
trusted native adapters. Core denies unsafe code. The native ABI and loader are
the only unsafe subtrees and document every pointer, layout, lifetime, mapping,
and allocator contract at the operation that relies on it.

## Runtime policy and durable input

`Runtime::new` validates topology, payload, execution, and deadline groups
before constructing a downstream primitive. Zero widths, arithmetic overflow,
Tokio primitive maxima, inconsistent aggregate/per-item limits, and deadlines
above the 24-hour hard ceiling are rejected. Synchronous Waterfall continuations
also have a hard 256-listener per-slot ceiling so configuration cannot turn their
nested call contract into unbounded stack use. Accepted policy values are
trusted typed Rust state.

Resolved factory identity, configuration, requirements, Portable keys, stable
Local configuration keys, effect labels, native adapter metadata, and diagnostics are bounded at their owning
input boundary. JSON configuration is checked iteratively
for encoded bytes, depth, and node count before retention. The configured depth
cannot exceed 128 because compact serialization remains recursive. Rejected,
normalized, unpolled, and callback-returned owned JSON values enter an
iterative-destruction guard before fallible work can drop them.

Preparation reserves Fiber, desired configuration, attempt configuration,
maximum prepared-state bytes, and worst-case requirement capacity before plugin
code. Factory identity is supplied by the resolver, bounded, and charged
independently for the Fiber lifetime; executable plugin code cannot change it.
Each desired configuration is validated once at its owning
input boundary and retained as typed proof; the Runtime reuses that proof when it
borrows the value into later preparation attempts. Plugin code cannot replace it
or receive a previous attempt's normalized value as input, and every normalized
output is independently validated before retention. Distinct desired and attempt allocations are charged
independently for as long as they coexist; an allocation shared by
Runtime-owned wrappers is charged once and its reservation follows those
wrappers. Core makes no accounting claim for a trusted safe-Rust plugin that
clones configuration into ownership the Runtime cannot observe. Core measures
normalized configuration and requirement metadata. The `retained_bytes` supplied
with opaque safe-Rust prepared state is a trusted in-process factory contract and
must include everything retained solely by that state. A prepared value is
Runtime-bound, desired-revision-fenced, and single-use. A stale or foreign value
cannot activate. The declared state charge remains reserved until the attempt
retires even after activation takes the value, since core cannot prove whether
the plugin dropped it or transferred it into generation-owned state. Core does
not claim byte-exact early release. Preparation has no Context or injected
capability, preventing dependency use before the requirement set exists.
Public preparation admission is fail-fast. A Runtime-owned reconciliation that
refreshes an already-admitted Fiber instead waits for a transiently full
preparation gate while yielding its reconciliation slot; capacity pressure is
not plugin failure and cannot retire the healthy installed generation.
The public `PreparedPlugin` proof retains Runtime admission and its pessimistic
resource reservations until apply or Drop. A caller that keeps an unapplied
proof across shutdown therefore keeps quiescence intentionally incomplete.

Plugin preparation and activation are unwind-contained through return-value,
future, state, and panic-payload destruction. Rejected and replaced values are
extracted under locks and destroyed only after those locks are released. A
private owner guard applies the same activation-future destruction boundary when
deadline, cancellation, or caller loss selects another branch.

Before plugin activation entry, core uses the same checked nonwrapping call-ID
allocator as Portable service dispatch to mint one nonzero lineage seed. It
installs that seed in the activation Context with the current Fiber as origin
and no parent. Exhaustion fails the Fiber before plugin code runs. Nested call
lineage is carried only by immutable Context values; it uses no thread-local or
Runtime-global ambient tracing state.

Reconfiguration reserves and prepares the complete replacement before publishing
its desired revision. Any error or panic releases only replacement reservations;
the installed desired value, Active generation, and requirement watchers remain
unchanged. Installing a successful replacement changes the desired value,
prepared attempt, and watcher set under one Runtime state transition.

## Profile source authority

`rsi-meta-profile` accepts an explicitly selected root Profile and follows
bounded file/include steps, including absolute paths. The Host process must
therefore already be authorized to read every selected source; successful
compilation and the resulting `source_digest` reveal that those bytes were
readable and parsed as a Profile. Profile loading is not a filesystem sandbox
or an allowlist. It rejects symlinks and special files at its owned reader
boundary before reading, verifies the opened regular-file identity, caps source
and group depth, count, bytes, and total expression work across one rebuild, and retains
only bounded redacted diagnostics. The digest includes native path bytes and is
therefore a host-platform-scoped identity, not a portable cache key. Products
that need a path policy place it before Profile source selection.

## Contexts and effects

Context isolation builders account each retained key and container rather than
estimating only payload values. Product scope and settings are explicit typed
modules above core; Context contains no arbitrary extension value or intercept
map that can become ambient authority.

Every mutable plugin operation validates Runtime, Fiber, generation, and
transaction state while holding the owning state lock. User setup, cleanup,
notification, Local object/event, Portable service, and listener callbacks run without Runtime or
scope-store locks.

An `EffectTxn` reserves and installs its wrapper before user setup. Defer
records ownership before a helper returns observable state. Commit, abort,
explicit disposal, unload, panic, and dropped-owner paths claim the same
one-shot records. Unloading rejects a new transaction and commit, but the
original owner of an already-open setup retains only the bounded authority to
defer its exact newly acquired undo before abort, Drop, or failed commit closes
it. The Runtime joins that setup or rollback and does not claim to reverse an
external action performed before its undo was deferred.
Each cleanup invocation and each caught panic-payload destruction has its own
unwind boundary. A payload destructor panic becomes bounded cleanup evidence
and cannot abort the remaining last-in, first-out undos.

`caller_effect` carries the exact caller Fiber and generation. It cannot be
retargeted, serialized, used after closure, or retained by a native callback
beyond an issued host capability's lifetime.

## Local and Portable supplies

A Local slot is exact contract TypeId plus Local isolation. Dynamic provision
atomically checks the owner generation, reserves capacity, rejects an occupied
slot, creates a non-repeating `LocalSupplyId`, inserts the Loading supply, and
advances the registry revision. Active lookup clones an `Arc`; core does not
inspect, serialize, meter, revoke, or drain arbitrary safe-Rust object state.

A Portable slot is the complete service key plus Portable isolation. Its
provision uses the same atomic ownership rules and additionally publishes a
bounded endpoint behind generation-fenced capability admission.

External lookup and injection require the provider to be Active and retain the
exact `SupplyId`. Call opening acquires provider admission, then revalidates
caller generation, provider generation, supply identity, and binding before it
allocates channels or starts a driver. Withdrawal removes external visibility
and closes admission while the same Runtime-state transaction cancels every
affected Loading generation and queues its exact reconciliation. User-owned
destruction begins only after that invalidation fence. Closure and the
admitted-callback count share one atomic state, so a racing callback either
commits before closure or observes it.

`Message` validates byte length and capability count before borrowing either
input. Queue admission reserves channel position, bytes, and queued capability
references as one all-or-nothing operation. A blocked sender owns only a
bounded, Runtime-accounted pending-send reservation until that transaction can
commit. Unique Runtime-wide capability entries are reserved when authority is
minted, not by infallible safe-Rust handle cloning or transfer. Safe Rust can
possess and transfer only an opaque `Capability`; it has no raw identifier,
reconstruction, kind, or rights state. Stale and foreign owned handles fail
before dispatch. Hostile raw-ID validation belongs to the independent native
ABI and Loader tables at that unsafe adapter boundary. `Capability`
diagnostics expose only bounded logical service/provider facts.
Generation retirement closes every capability entry before draining any
admitted use and yields its reconciliation slot while those deadline-bounded
call drivers finish, so stale authority is fenced without blocking unrelated
Fiber convergence.

Mixed-size byte admission is work-conserving with bounded bypass: a fitting
message may use otherwise idle capacity, but an older request becomes the
reservation barrier after 64 younger grants. Every logical reservation is
released before returning its admission capacity so an awakened waiter cannot
observe transient false room. Pending waiters use keyed removal and bounded
per-channel candidate windows. Registration or cancellation schedules only
when it exposes a newly fitting candidate, returns capacity, or removes a
fairness barrier; concentrated cancellation cannot repeatedly scan unchanged
nonfitting work. Registration into a full channel window displaces its youngest
nonfitting candidate when the new waiter fits current global capacity, keeping
the window constant without hiding usable capacity.

The call driver owns the real channel halves, absolute deadline, cancellation,
provider lease, caller lease, and unique terminal. Providers borrow a channel
for the callback lifetime. Provider-future destruction occurs before terminal
publication and lease release. A clean terminal alone means EOF. Terminal
observation destroys the response inbox immediately, releasing late queued
Messages even if the public call value remains live, while the bounded terminal
result remains cached so later reads cannot turn an error into clean EOF.

## Local events and scoped storage

Event registration, once claiming, explicit disposal, activation rollback, and
unload share one owner token. Dispatch snapshots the exact event TypeId and
Local isolation, then releases the registry lock before invoking callbacks.
There is no user selector, global bypass, ancestor traversal, common dispatch
deadline, or Runtime-owned callback driver.

`LocalEventHandle::dispose` withdraws registry membership but is not a drain
fence for ordinary bindings already retained by a dispatch snapshot. Once
listeners are claimed through their exact effect ownership after selection, so concurrent
disposal can prevent a once callback rather than leave duplicate authority.
Listener destruction happens after Runtime locks are released. If user-owned
listener state panics while being destroyed, the exact removal still publishes
completion, the failure is retained in its cleanup report, and the Runtime
terminalizes because safe continued execution is no longer provable.

Each event marker fixes one typed dispatch mode. Parallel callbacks are
caller-owned Futures and are consumed all-settled; the other modes preserve
their documented ordering and typed break behavior. Panic and Drop semantics
are ordinary safe-Rust semantics rather than a serialized foreign boundary.
Parallel claims a once listener only as that callback enters the bounded
polling window, so cancellation cannot spend unpolled once authority.

`rsi-meta-scope` keys belong to one `ScopeRoot` and cannot cross roots.
Parent cycle check and link replacement are one critical section. The library
does not prove the product's quiescence precondition for rebind. Layer callbacks
run after unlocking. Snapshots clone bounded owned values rather than exposing
a lock guard or iterator into mutable storage.

A fallible add notification runs after the value is visible. Its failure
triggers exact undo and a compensating notification. Both errors are retained
when both fail. Removal notification failure never restores removed state.
Built-in entry insertion records its exact undo with the active layer action
before returning to product code. An action error or panic after insertion
therefore aborts through that recorded undo instead of leaving silent state.

## Native trust and ABI v3

Native plugins are trusted process code. A loaded artifact can read or corrupt
memory, access the operating system, spawn untracked threads, or abort the
process. Hashing identifies the mapped top-level bytes; it does not sandbox the
plugin or authenticate its transitive operating-system dependencies. Products
that need artifact policy place it in front of explicit native-loader authority.

ABI v3 has no earlier-version compatibility path. The native loader treats every native table,
frame, pointer range, token, status, and output as hostile until it has validated
and adopted the value into typed Safe Rust ownership. The maintained
[`rsi_meta_plugin.h`](../native/include/rsi_meta_plugin.h) owns the exact raw
layouts, validation order, statuses, operation state machines, and one-shot
release rules.

Native callback frames are host-owned authority. Callback-scoped channels and
effect transactions are sealed when foreign code returns and cannot become
durable product state. Explicitly transferable capabilities retain the mapped
module and issuer authority until their safe owner releases them. No
capability-table, cache, or Runtime lock is held while entering foreign code;
Native-loader admission rejects reentry or contention instead of waiting across that
trust seam.

A native create or call timeout atomically poisons the adapter and terminalizes
the owning Runtime before foreign completion can release its gate. Native code
cannot be forcibly stopped safely, so timeout seals later admission but retains
the callback thread, frame, library, capabilities, effect ownership, instance
gate, cache lease, and accounting until foreign code actually returns.

## Native mapping and shutdown

The native loader owns a dedicated content-addressed staging cache and its callback,
instance, destruction, staging, and durable-byte limits. It accepts regular
non-symlink files, hashes through bounded private staging, validates ABI before
mapping, and records the digest of the exact stable copy. It performs no package
discovery or version resolution. Unix operations resolve
against the pinned and locked directory object; Windows support requires an
exclusive private writer followed by a read-only handle denying write and
delete sharing. The native-loader README owns the complete
cache and finalization contract.

Runtime completion is published only after external admission is closed and
drained, the scheduler is idle, the Fiber registry is empty, and every logical
resource counter is zero. Timeout returns tracked unresolved ownership and
never changes those conditions. Native platform behavior is claimed only on a
host where the real dynamic-library conformance suite ran.
