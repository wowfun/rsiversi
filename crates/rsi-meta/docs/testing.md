# rsi-meta testing

Tests exercise the public `Runtime`, `Context`, `PluginFactory`, service, event,
plugin ABI, and Loader interfaces. They assert behavior and resource ownership,
not private implementation shape. Evidence is organized into four stable
families; individual test names are deliberately not indexed here.

## Lifecycle and resource behavior

Accepted grouped topology, payload, execution, and deadline limits must be safe
for every downstream primitive. Invalid zero values, arithmetic overflow, Tokio
primitive maxima, and deadlines over 24 hours fail at construction. Logical
reservations include staged and published ownership, release exactly once, and
remain reusable after release; snapshots report current use, high-water marks,
rejections, and unresolved cleanup rather than allocator RSS.

Preparation is fail-fast, Runtime-bound, and reserves capacity before expensive
or retaining work. Dropping a proof releases it, applying it in another Runtime
fails, and applying it in its owner transfers the existing normalized descriptor,
configuration, indexes, and reservations without repeating normalization.
Application and reconfiguration publish services, listeners, configuration, and
ownership atomically only after activation succeeds. Reconfiguration keeps the
old configuration bytes reserved while a cancelled or completing activation
still owns that shared value.

Apply, reconfigure, service changes, dispose, and shutdown converge through the
bounded per-Fiber scheduler. Repeated revisions coalesce, one Fiber has at most
one active transition, and yielding a reconciliation slot while joining nested
Fiber work prevents a single-slot deadlock. A service revision that invalidates
a loading attempt cancels it before provider cleanup joins that Fiber, including
when activation reentrantly awaits that provider's disposal. Revision saturation
still treats each queued intent as work; a run settles only the ticket batch it
captured before reconciliation, and later tickets wait for the queued rerun.
Service-change notification follows the complete service-isolation slot, so a
same-named provider in another isolation cannot create a false dependent wait.
Declaration-only insertion and removal refresh cycle diagnostics without
cancelling an in-flight activation bound to a real provider, and removing a
cycle participant eventually removes the stale cycle reason. Destroying an
inserted but unacknowledged apply waiter on a non-Tokio thread still reaches
disposal through the executor captured at insertion.
Concurrent reconfiguration is
fail-fast `Busy`; disposal is idempotent and joinable. Once admitted, transition,
rollback, LIFO cleanup, disposal, and shutdown work remain Runtime-owned when a
waiter is dropped or its absolute deadline expires. Shutdown timeout returns a
bounded unresolved snapshot while the same run continues. Only a hard-sealed,
drained admission gate, idle scheduler, empty registry, and zero logical usage
produce the cached `Complete` outcome.
Tests also require a disposal path that terminalizes after a late panic to
settle every reconciliation ticket registered before disposal was requested.
When a pending-report prefix leaves no capacity for a cycle service, the cycle
still increments `total_reasons` and `truncated` but does not retain an empty
`DependencyCycle` placeholder.
Cleanup reports retain a bounded prefix in observation order and maintain their
aggregate UTF-8 byte count incrementally, so collecting or joining the maximum
accepted diagnostic cardinality remains linear. Serialization omits that private
ledger while deserialization reconstructs it; contradictory retained counts or
truncation metadata are rejected, and equality depends only on the public report
value.
Context overlay tests account JSON escaping and container delimiters exactly.
Isolated-process tests require configuration, intercept, event input, plugin
normalization output, and event-handler output to reject or destroy
adversarially deep owned values without stack exhaustion, including futures
discarded before their first poll.

## Service behavior

Service lookup and call opening enforce exact contracts, caller and provider
generation fences, and direct-edge authority. Opening a call reserves the global
call slot and bounded bidirectional channels before invoking the provider; queued
frames retain weighted byte permits until consumed or dropped. The caller keeps
call admission until it observes the unique terminal or drops its
`ServiceCall`. Mixed-weight evidence keeps one large frame waiting behind
partial occupancy and proves an independent fitting frame can use the remaining
Runtime budget, while repeated bypass evidence proves the older frame receives
the next released capacity after the documented bound.
Synchronous opening from a thread with no entered Tokio context uses the caller
Fiber's still-live executor without invoking the process panic hook; async call
operations remain subject to the package-wide time-enabled polling contract.

The provider receives a borrowed, non-cloneable `ProviderChannel<'_>`. Send,
receive, cancellation, and the complete call share one absolute deadline. Only a
clean terminal becomes EOF; endpoint error, panic before or after frames, Runtime
terminalization, and caller cancellation remain distinguishable bounded errors.
Runtime-terminal and absolute-deadline selections remain authoritative when
destroying the losing endpoint future panics; cancellation and provider-result
paths still classify that panic as `ServiceEndpointPanicked`. Observing a
terminal destroys the response inbox immediately, so late buffered frames cannot
retain byte reservations through shutdown merely because the `ServiceCall` value
remains live. Endpoint-future destruction stays synchronous before terminal
publication and provider-lease release, and tests cover the resulting tracked
blocking-`Drop` boundary.
Provider panic is call-local and does not by itself retire the generation.
Retirement closes admission, converges dependents, drains admitted callbacks,
and then cleans up without freeing resources still reachable by a live call.

## Event behavior

Listener registration, once-claiming, explicit removal, rollback, and unload
share one generation-fenced ownership model. Removal clears the registry,
event bucket, owner set, and staged state together; empty buckets and stale owner
history must not accumulate. Dispatch snapshots immutable listener bindings and
shares validated immutable input rather than cloning it per listener.

Dispatch and callback admission are separately bounded. `emit`, `serial`,
`waterfall`, and `parallel` share one absolute dispatch deadline and the same
global callback budget. Parallel dispatch admits work lazily and consumes each
outcome as it completes, so a slow sibling does not retain completed results or
callback permits. Handler output, panic/error diagnostics, and aggregate results
obey entry-count and UTF-8 byte bounds.

## Native and Loader behavior

Plugin ABI evidence crosses a real host dynamic-library boundary and covers table
layout and version direction, null/zero inputs, allocator-matched buffer release,
partial-instance ownership, panic containment, outbound host service calls, and
load/call/destroy behavior. Native artifacts are trusted in-process code, so a
create or call timeout terminalizes the Runtime while the catalog continues to
own the worker and resources until foreign code actually returns.

`NativeCatalog` exclusively owns a dedicated cache, accepts only managed digest
files, streams and hashes through bounded staging, validates entry and descriptor
before durable commit, and accounts cache artifacts, cache bytes, staging bytes,
live instances, callbacks, and destruction before starting the corresponding
work. Symlinks, special files, collisions, quota exhaustion, failed validation,
and final-digest mismatch fail closed. Cold same-digest contenders fence before
staging and share one staging charge; source mutation rekeys the stable copy to
its authoritative digest instead of coalescing mismatched bytes. Every failed or
timed-out worker removes owned staging before releasing the last cache lease. A
deterministic timeout state-machine test holds timeout publication in
progress and proves callback completion cannot release the foreign gate before
poison and terminal state are visible. Callback activity includes result
delivery and worker-closure exit, so a returned load result is followed by a
bounded quiescence wait before asserting zero activity. Unix tests unlink the
lock marker while one Catalog is live and prove that
the pinned directory lock still excludes a second owner. A path-replacement
rollback removes and fences only the claimed directory's publication, never a
same-named replacement entry. Callback admission occurs before an OS thread is
spawned; per-instance serialization, a reserved destruction lane, bounded
live-instance slots, and a physically bounded fallback queue keep teardown
joinable without consuming Tokio's shared blocking pool.

On Windows, the private staging writer denies sharing; the catalog reopens it
read-only with write/delete sharing denied, revalidates length and digest, and
retains that handle through unload. Platform support is claimed only where CI
actually builds and runs the real native fixture, including load, call, destroy,
and cache behavior. Unix capability tests replace and restore the public cache
pathname without timing races and prove that scan, temporary, open, publication,
comparison, and rollback still address only the pinned directory. Loader initial
mapping, Runtime preparation, and command admission are bounded before child
publication or ID ownership; tests cover sequential normalization for one shared
native module, Runtime-limit-one normalization for distinct modules, and
generation-fenced rollback after an ID is reused. Command responses, including maximum-
cardinality inspection, encode through the Runtime frame budget and return a
bounded error instead of publishing an oversized frame.

## Standard validation

For a complete foundation change, run:

```sh
cargo fmt --all --check
cargo clippy --locked -p rsi-meta --all-targets -- -D warnings
cargo clippy --locked -p rsi-meta-plugin --all-targets -- -D warnings
cargo clippy --locked -p rsi-meta-loader --all-targets -- -D warnings
cargo test --locked -p rsi-meta --all-targets
cargo test --locked -p rsi-meta-plugin --all-targets
cargo test --locked -p rsi-meta-loader --all-targets
cargo xtask rsi-meta code-health
cargo xtask verify-docs
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --lib --no-deps
```

`cargo xtask rsi-meta conformance` runs the package-level rsi-meta evidence.
The Linux repository conformance job also lints and executes the standalone
release-mode cycle probe; its public lifecycle and scalability assertions must
run, not merely compile. Native execution evidence applies only to the host
platform exercised; Windows and macOS results are not inferred from Linux runs.
