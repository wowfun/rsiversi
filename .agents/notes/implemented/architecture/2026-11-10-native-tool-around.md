---
name: Native Tool around callbacks belong to the Agent composition
---

## Problem

The application Tool bridge can wrap an admitted native body, but native Agent
plugins could register only policy decisions and post-settlement callbacks.
Neither expresses an awaited around callback. Implementing that behavior through
the application source would require a JS request capture and native Tool socket.

DSH's `packages/core/tools/src/pipeline.ts::dispatch` awaits its scoped
`tools/execute` waterfall. Cordis calls each listener with a continuation and
rejects a second call dynamically. These are useful execution semantics to port;
neither the JS object graph nor a second runtime is needed for native callbacks.

## Decision

The existing Agent contribution registrar admits `ToolAround` callbacks with
caller-owned Meta registration positions. A composition pin retains their order
and lifetime. The executor installs one native chain in the Tool's existing
continuation slot after approval, before durable execution starts. Callbacks
themselves execute only inside the started Tool's retained-result scope.

A consuming continuation supplies borrowed arguments, the exact Tool name and
execution authority. Rust ownership prevents invoking that continuation twice.
The chain retains the generation owner, borrows the existing catalog and moves the body rather than copying
arguments or allocating a callback vector. There is one wrapper per participating
invocation, ordinary async-trait callback futures, and no extra task or IPC. An
empty native chain leaves the original execution plan untouched.

The native chain wraps the existing application continuation. That explicit seam
does not promise to merge native and JS registrations into Cordis's global event
order. A built-in with ordering dependencies cannot be moved across that seam
without an independent behavioral proof.

The owning contract is the
[composition protocol](../../../../crates/rsi-agent/composition-protocol/README.md).

## Alternatives considered

A separate native hook runtime would duplicate the existing registration and
generation owners. A generic asynchronous Meta event mode is premature while
this use requires Tool-specific start, approval and receipt boundaries. Adding
another `RequestToolSource` for native hooks would preserve the wrong abstraction:
that source describes an application request and its supplemental catalog.

An independently spawned hook task would complicate cancellation and result
retention. Mutable call arguments would invalidate the existing policy and
approval decision. A dynamic duplicate-next flag is unnecessary for the native
consuming continuation.

## Consequences

The Tool owner continues to own timeout, cancellation draining, panic capture,
and the final result. Native hook results use the same identity and durable
publication as the body. Existing pins retain removed/replaced callbacks;
new generations observe the new catalog.

The exercise also reproduced an existing cancellation gap: an around callback
could wait for cancellation and still enter an unstarted Tool body. DSH's
`dispatchToolBody` rechecks at that last boundary. `ToolBody::run` now does the
same; this preserves draining of work that has actually started.

This establishes native authoring support. It does not yet convert the Standard
profile's JS built-ins or remove its always-present application input capture.
