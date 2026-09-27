# Events and scoped contributions

## Local events

Each `LocalEvent` marker fixes its argument, output, and one of the five dispatch
modes in its type. Callers dispatch the marker and cannot select a mode at the
call site. Emit is synchronous ordered fail-fast; Parallel is asynchronous
all-settled with aggregate errors; Serial is asynchronous ordered first-break;
Bail is synchronous ordered first-break; Waterfall is a synchronous typed
continuation chain. Because Waterfall's around/short-circuit contract requires
nested continuations, each exact Waterfall event slot has its own configured
listener ceiling in addition to the Runtime-wide listener ceiling. Runtime
construction rejects a per-slot ceiling above the hard stack-safety bound of
256 listeners.

Listener registration is an immediate effect-owned mutation, including while a
generation is Loading. Append is default; prepend and atomic once are explicit.
Registration, once claiming, explicit disposal, activation rollback, and Fiber
retirement share one generation-fenced removal transaction.

Dispatch snapshots exact `(TypeId, LocalIsolationId)` listener bindings and
releases registry locks before callbacks. There is no target callback, ancestor
selection, or global bypass. An ordinary binding already present in the
snapshot remains eligible for that dispatch if its handle is concurrently
disposed; disposal removes it from future snapshots. A once binding still
requires its exact atomic claim and cannot run twice across concurrent
dispatches.

The registry orders listeners by their owning composition position, then by
registration order within that exact position. The prepend lane precedes append
and reverses declaration order; append follows declaration order. One dispatch
captures immutable membership and ranks before calling any listener. Unchanged
membership/order reuses the same sorted snapshot. Exact-handle removal remains
indexed by listener identity.

`Context::child_position` reserves an opaque stable identity for one direct
child. `with_child_position` derives a Context selecting that identity for apply;
an ordinary apply reserves its position at admission. At most one live Fiber may
occupy a position. Rebuilding after disposal may reuse it. Positions belong to
one Runtime and exact parent generation; another parent or stale generation
cannot apply or reorder them. `reorder_children` atomically publishes ranks for
that parent's positions, placing explicitly listed positions first and retaining
the relative order of omitted positions afterward. It never changes Fiber or
registration generations. Descendant order follows the complete position path.
Ordinary concurrent child admission or registration inside a single plugin does
not promise deterministic order across runs.

The Runtime bounds simultaneously retained position identities separately from
Fiber capacity. A position retains only bounded composition metadata and its
ancestor positions, not execution admission or a Runtime reference. Explicit
position handles can therefore outlive shutdown as stale metadata. Last-owner
release removes the exact order entry; there is no historical position table.
Candidate and compensation handles may coexist without reserving candidate
Fibers. Rank publication does not introduce another lifecycle graph.

Local callbacks execute directly. Runtime does not add a common deadline,
spawn, cancellation token, call identity, or dispatch resource tracker. Errors,
panics, and typed breaks follow ordinary safe-Rust semantics and the marker's
mode. Parallel constructs one caller-owned all-settled Future from the exact
snapshot, preserves listener order in its aggregate errors, and polls at most
64 callback futures concurrently. A Parallel once binding is claimed only when
that dispatch admits its callback into the polling window; dropping the Future
does not consume bindings that the window never admitted.



## Scoped contributions

`rsi-meta-scope` is a library above core, not a Runtime service. The retired
generic `ContextExtension` metadata facility was removed without migration to
Local contracts; product-specific immutable metadata belongs in an explicit
typed wrapper or owning module. A
`ScopeRoot` is constructed with an explicit maximum complete ancestry depth in
the inclusive range `1..=4,096`; the key itself counts toward that depth. The
root mints opaque `ScopeKey` identities and serializes parent-link changes, but
it does not retain a registry of minted keys. Each live key owns its parent
link and non-owning child links support depth validation without retaining
descendants. Bind and rebind reject an edge before it would make any key in the
moved subtree exceed the root's configured depth. A child that has never been
a parent cannot occur in a proposed parent's ancestry, so ordinary leaf
attachment performs no ancestor walk; the monotonic proof is discarded only
by dropping that key. A possible cycle is still checked to the root. A child
or parent binding retains only the ancestry it can still observe, and otherwise
dropping the last key iteratively reclaims the complete unreachable chain
without a root-side sweep, recursive final destruction, or historical-key
bound.
`ScopedContext { Context, ScopeKey }` is the explicit product-facing wrapper.
Scope-owned registrations use its owning Fiber effects; scope never becomes a
generic Context extension and does not participate in Local event targeting.

`ScopedLayers` owns one eager global layer and lazy exact-scope aggregate
layers. Effective named snapshots apply global values, then ancestor overlays
from farthest to nearest; nearest same-name values win without moving unrelated
entries. Overlay resolution retains shared entry values and clones only the
final visible owned snapshot. Exact `peek` validates only root identity and does not walk ancestry;
reads never create a layer. `NamedEntries` and
`AnonymousEntries` preserve insertion order and exact independent ownership.
Explicit ordered contributions use `ScopedContributions` instead. Its owner
supplies one Runtime identity, one ScopeRoot and a total live-entry bound.
Registration takes the caller's narrow `RegistrationContext` and an explicit
optional scope key; the key selects visibility and grants no generation
authority. Both Runtime and scope-root mismatches reject before publication.
Loading joins setup undo and Active owns a dynamic effect. Each entry is
removed by its exact lease or generation retirement.

An ordered contribution snapshot includes global entries, then matching
ancestors from farthest to nearest. Each layer follows current composition
declaration order; this does not change named overlay replacement. Snapshot
capture retains one immutable Arc, and business callbacks run after capture.
The table caches only its last bounded selection, avoiding a history of queried
scope keys. Unchanged membership, ancestry and effective order reuse that Arc,
including after an unrelated order publication. Reparenting affects the next
capture; existing snapshots keep their selected values. Entry removal also
releases its exact scope reference, and query-only reads create no layer.
Each product store declares its maximum simultaneously retained exact-scope
layers. An existing key remains usable at saturation, while a new key fails
before factory execution; a cleanup failure may consume capacity but cannot
turn repeated distinct-key churn into unbounded retained history.
Each layer's reclamation ABA version saturates instead of wrapping. The layer
continues to accept capacity-bounded mutations after exhaustion, but that exact
slot permanently fails closed against automatic reclamation.
Failed lazy materialization removes its exact uninitialized cell before
returning, so a panicking layer factory cannot retain the scope key or create
root-like history in the product store.

The original `ScopeParentBinding` is the only rebind authority. Rebind checks
same-root identity, subtree depth, and cycles atomically but neither proves
quiescence nor notifies product registries. A product that retains derived
state owns that precondition and notification.

Layer mutation and visibility derive from one Context. Add becomes visible
before the fallible change callback. If that callback fails, exact undo and a
compensating callback follow; the first failure remains authoritative. Removal
never resurrects an entry when its notification fails. User callbacks never
run while a store lock is held, and reads return owned snapshots. Every caught
lazy-factory, action, change-callback, undo, or reclamation panic also destroys
its panic payload behind a second unwind boundary. A panicking payload
destructor becomes bounded failure evidence and cannot escape or skip the
remaining exact undo, reclamation, or notification path. Change futures are
both polled and explicitly destroyed inside that containment before their
result is published. Their owner guard applies the same destruction boundary
when the mutation waiter is cancelled while polling; the dropped open
`EffectTxn` still transfers exact undo and notification to Runtime-owned abort.
Built-in entry stores publish each new exact undo to the surrounding action
transaction before returning it to product code, so an action error or panic
after insertion cannot strand an unowned visible entry.
