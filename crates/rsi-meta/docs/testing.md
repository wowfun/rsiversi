# rsi-meta testing

Tests exercise public `Runtime`, `Context`, `FiberHandle`, factory,
effect, Message/capability, event, scope, ABI, and Loader seams. They assert
observable state and resource ownership, not private container shape. Failure,
cancellation, timeout, panic, saturation, and teardown are first-class paths.

The implementation proceeds in vertical red-green slices. A slice is complete
only when its closest public behavior test fails for the intended reason before
the code change, passes afterward, and its resource snapshot returns to the
expected baseline.

## Preparation and lifecycle

Evidence covers:

- bounded, Runtime-bound, single-use preparation with no Context or dependency
  access;
- every attempt preparing from the retained desired configuration rather than
  the preceding attempt's normalized output;
- per-attempt injection derived together with that attempt's normalized
  configuration;
- exact once-only identity capture and independent Fiber-lifetime identity
  accounting;
- opaque prepared state transferred once into activation, wrong-type take
  preserving it, trusted state-byte admission, redacted diagnostics, and
  contained destruction on rejection, replacement, cancellation, and disposal;
- activation future construction, polling, normal and cancellation-time future
  destruction, and recursive panic-payload destruction all remaining inside one
  private driver while setup authority stays live;
- Pending on missing actual supply without calling activation;
- Active supply appearance causing one activation;
- stale desired revision or changed `SupplyId` fencing a blocked Loading
  attempt; resolve-to-Loading reconfiguration or disposal fencing installation;
  joining rollback, rerunning preparation, and activating only the new plan;
- preparation error or panic retaining no Context, effects, listeners, supply,
  attempt, or topology resource;
- mutual missing requirements reporting missing values without a fabricated
  dependency cycle;
- coalesced revisions, revision-counter saturation, nested transition-slot
  yield, bounded independent concurrency, one scheduler worker, and disjoint
  FIFO ready/active/rerun frontiers under mass active re-requests, including a
  rerun that cannot overtake an already-ready Fiber and an internal refresh that
  waits for the sole preparation slot without turning transient pressure into a
  terminal Fiber failure;
- fail-fast concurrent reconfiguration, generation-fenced stale Contexts, and
  atomic replacement of desired value, prepared attempt, and requirement
  watchers;
- reconfiguration preparation failure preserving the old desired revision and
  Active generation, plus exact accounting for old/new desired and attempt
  allocations through coexistence;
- caller timeout and dropped waiters leaving admitted apply, reconfiguration,
  rollback, disposal, and shutdown joinable;
- generation capability drain yielding bounded reconciliation capacity while
  an admitted call remains in flight.

Dense-graph and lifecycle probes use deterministic gates rather than sleeps.
Their elapsed times, scaling ratios, and snapshot latency are report-only;
conformance does not turn host scheduling noise into a correctness failure.
Every terminal disposal path settles already registered reconciliation tickets,
including a caught late panic.

## Effects and dynamic supply

Wrapper-first tests reproduce reentrant unload during setup: unload observes the
SettingUp record, waits for setup or failure, and runs every registered undo
exactly once in reverse order. Setup error, panic, dropped open transaction,
explicit abort, explicit effect disposal, and owner unload converge on the same
cleanup record. Cleanup panic-payload destructor tests prove that hostile
payloads remain bounded and do not skip later sibling undos. Begin and commit
after Unloading are rejected; the original
owner of an already-open setup may still defer its exact undo before closing,
but a closed or stale transaction cannot mutate a later generation.

Dynamic supply tests cover:

- Loading slot occupation and duplicate rejection;
- provider self-lookup during Loading while external lookup, injection, and
  call opening remain unavailable;
- activation failure withdrawing the internal supply without ever binding a
  consumer;
- Active add waking a Pending consumer;
- withdrawal closing admission, converging exact dependents, draining an
  admitted call, and dropping the endpoint in order, including a blocked
  listener destructor proving that no Loading dependent can publish after
  visibility is removed;
- same-generation withdraw/re-provide minting a new `SupplyId` and rejecting
  the old handle;
- isolation changes notifying only the complete matching slot;
- owner failure, resource saturation, notification races, and final zero
  service/call/effect resources.

`caller_effect` tests register a product contribution from a provider callback
and prove it follows caller disposal, caller reconfiguration, cancellation,
provider retirement, and stale-generation rejection.

## Messages and capabilities

Call tests use both streaming `open` and exact-unary `invoke`. They cover nested
transferred capabilities, clone/drop sharing of one accounted entry, foreign
Runtime possession, stale owned handles, generation retirement, unique-entry
limits, and queued-reference limits. Safe Rust has no raw-token or import
interface. Native ABI tests exercise their independent hostile `CapId`
boundary separately.

Nested service/event tests distinguish the current, immediate-parent, and root
lineage call identities. The activation plan exposes the nonzero root seed; the
first service callback has a distinct current ID and no parent. Re-entering an
earlier provider must retain the activation seed while each nested hop reports
its exact immediate `parent_call_id`. Counter exhaustion must fail before
plugin activation entry.

Unary evidence requires one request, one response, and clean EOF. Zero
responses, extra responses, error or panic after one response, absent terminal,
cancellation, and timeout are all failures. Streaming evidence covers
bidirectional backpressure, finish semantics, caller and provider cancellation,
late losing sends, endpoint-future panic and blocking Drop, and opening without
an ambient Tokio context.

Channel position, byte, and capability admission are tested as one transaction
at exact per-message, channel, Runtime, and pending-sender saturation. A
budget-blocked sender must not occupy its destination channel or block a later
fitting message. Mixed-size tests prove bounded bypass remains work-conserving
and starvation-free. Deterministic structural tests populate the default
65,536 pending-sender scale, prove that one channel exposes only the constant
65-entry scheduler window, prove a fitting registration remains visible when
that window is full of nonfitting candidates, and prove mass cancellation of
unchanged nonfitting candidates performs no scheduler scan. Receiving,
dropping, terminal observation, cancellation, and shutdown each release
retained Messages, waiters, and handles exactly once.

## Events

Loading listener tests dispatch while activation is blocked, then force
activation failure and prove rollback removes the listener and reservation.
Registration, once claiming, explicit disposal, rollback, and unload are raced
against dispatch under one owner token.

Target tests cover no-target select-all, matching and nonmatching listener
views, explicit global bypass, false selection preserving once, selector error
and panic before any callback, selector reentrant Runtime access and listener
registration without deadlock, snapshot ordering, and a deterministic
selection barrier proving that concurrent disposal preserves only the current
ordinary snapshot. The same barrier covers a rejected snapshot whose final
listener destructor panics after concurrent removal. Selector and listener
callbacks never run under the registry lock. A separate blocking-selector
barrier proves the absolute dispatch deadline returns before selector release,
starts no later selector or callback, and retains bounded dispatch ownership
until the blocking worker really exits. Exact-dispose tests also use a
panicking listener destructor to prove that removal completion and its bounded
cleanup failure remain joinable.

Callback-boundary tests use handwritten `EventHandler` implementations to
cover panic during future construction, recursive panic-payload destruction,
completed-future Drop registration through `caller_effect`, and future Drop
panic. They prove returned JSON is iteratively owned before teardown and the
callback lease closes only after future and panic-payload destruction.

Emit, serial, waterfall, and parallel modes share deadline, input, output,
diagnostic, dispatch, and callback bounds. Parallel tests prove lazy admission,
completion-order resource release, sibling independence, and bounded aggregate
failure collection.

## Scoped contributions

`rsi-meta-scope` tests its public API independently and through a real
Context/Fiber:

- one eager global layer, lazy exact-scope layers, non-creating reads, and
  aggregate reclamation only when every table is empty, including exact-cell
  reclamation after a lazy layer factory panic and exact-scope capacity
  rejection/reuse when reclamation itself fails, plus fail-closed version
  exhaustion without ABA wrap;
- root-local opaque keys, explicit ancestry-depth validation at construction
  and topology mutation, async scope creation, inherited Context extension,
  nearest-first parent chains, duplicate bind, self-cycle and ancestor-cycle
  rejection, iterative deepest-key destruction on a 64 KiB stack, and proof
  that the root retains no dropped key or unreachable parent-chain nodes;
- unique binding authority and atomic old-or-new parent snapshots during
  rebind, with no implicit registry notification;
- `ScopeTarget` exact-key and ancestor routing, unscoped listener admission,
  foreign-root rejection before callbacks, and one immutable chain snapshot
  across rebind;
- deterministic structural and operation-count probes proving that append-only
  leaf attachment performs constant ancestor work, a closing cycle is still
  traversed and rejected, target construction visits the bounded chain once,
  exact layer lookup performs no ancestor walk, and listener selection uses
  precomputed set membership rather than rescanning that chain;
- global then farthest-to-nearest named shadowing, stable unrelated insertion
  order, cloning only final visible effective values, caller-owned duplicate
  diagnostics, and independent equal anonymous values;
- owned snapshots that do not hold store locks across caller code;
- mutation visible before notification, first-notify failure causing exact undo
  then compensating notification, both-failure evidence, and removal-notify
  failure without resurrection, plus action error or panic after insertion
  retaining and running the exact undo;
- panicking scope callbacks and panic-payload destructors contained as bounded
  evidence, including recursively panicking payloads and change-future
  poll/destruction failures and cancellation while Pending, while exact undo,
  reclamation, compensation, and resource release still complete;
- effect ownership derived from the same scoped Context, idempotent disposal,
  activation rollback, and concurrent add/remove/read without callbacks under
  locks.

Product-specific validation, filtering, sorting, schema, and observer behavior
remain in product tests; the scope crate does not invent generic policy tests.

## Native ABI and Loader

ABI unit tests compile the C11 header and C++17 inclusion seam, validate fixed
layout and version direction, and cover null/zero combinations, arithmetic
overflow, mandatory pointers, malformed Messages, capability token issuer,
slot, epoch, kind and rights, operation state, output token ownership, and panic
containment. Every success and rejection path asserts one-shot release exactly
once.

Loader evidence crosses a real dynamic-library boundary on the executing host.
The happy fixture performs preparation, dynamic provide, one complete
bidirectional callback, nested capability use, effect defer/commit, activation
rollback, unload, and destruction. A v1-only artifact is rejected because no
fallback symbol exists.

Complete Loader evidence requires hostile fixtures for partial entry and create
ownership, malformed bytes/capability arrays, stale and foreign tokens, double
output release, callback-exit open-transaction autoabort, call-channel use after
return, provider/caller channel orientation, same-lineage same-instance reentry
across a different port returning `REENTRANT` before serialization, unrelated
contention returning `BUSY`, unregister/call races, and timeout retention until
foreign return. Completion also requires public callback, instance, module,
staging, cache, capability, effect, destruction, and Runtime resource snapshots
to reach zero only after actual teardown.

Table unit evidence distinguishes a duplicate release in the still-current
consumed epoch (`PROTOCOL_ERROR`) from an old token after slot reuse (`STALE`),
and uses deterministic allocator probes to prevent quadratic monotonic growth.
Gate barriers cover both admission paused between poison observation and claim,
and callback return paused before completion/timeout publication.

Catalog tests retain existing real filesystem coverage: stable-copy hashing,
same-digest contention, source mutation, symlinks and special files, cache and
staging quotas, durability failure, path replacement, pinned Unix directory
authority, Windows sharing behavior on Windows, bounded callback admission, and
reserved destruction/finalization lanes.

Native results apply only to the host that executed them. Linux evidence does
not establish native Windows or macOS behavior.

## Model and stress evidence

Small ownership state machines model effect setup/abort/unload, supply
admission/withdrawal, and slot-plus-epoch capability reuse. Model tests assert
one cleanup claimant, no admission after closure, no ABA acceptance, and no
resource release before foreign callback exit.

High-contention tests repeat concurrent provide/withdraw/open, effect
commit/abort/unload, event once/remove/dispatch, scope add/remove/rebind, and
shutdown admission. They use barriers, channels, and paused time rather than
fixed timing assumptions. Sanitizer or Miri evidence is additive and never
substitutes for the real dynamic-library suite.

## Standard validation

For the complete foundation cutover, run from the repository root:

```sh
cargo xtask rsi-meta conformance
cargo xtask rsi-meta code-health
cargo xtask verify-docs
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --lib --no-deps
cargo test --locked --workspace --doc
```

`cargo xtask rsi-meta conformance` owns the package, scope, ABI, Loader, and
standalone-fixture sequence so local and CI evidence cannot silently diverge.
After its root package checks have materialized the shared locked dependencies,
it loads each standalone manifest with locked offline metadata. It then runs
offline lint, test, release build, and release probe operations as applicable.
The echo fixture's unit test exercises its v2 table header; its Linux release
artifact must export only `rsi_meta_plugin_entry_v2`. The ABI package tests own
C11/C++17 compilation of the maintained public header. Repository CI
additionally runs the non-rsi-meta remainder of the root workspace; the
conformance command remains the only CI authority that enumerates rsi-meta
packages and fixtures. CI runs that native suite on each claimed platform.

The final implementation report lists every command actually exercised,
including iteration/stress counts and target triple, and records native Windows
and macOS as unexecuted when those hosts were unavailable.
