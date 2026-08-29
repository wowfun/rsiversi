---
name: Bounded credential resolution admission
comment: Bound synchronous secret-store work at the credentials owner
---

## Problem

Validated AI requests do not bound synchronous OS keyring work. Concurrent
preparation could issue an unbounded number of blocking reads, repeated calls
for one credential could duplicate the same lookup, and a stalled backend
could hold an async caller forever. The old AI Registry no longer exists, so
placing admission there would assign policy to a deleted and incorrect owner.

## Decision

`rsi-credentials-local` owns resolution admission because it owns the
synchronous `SecretStore` boundary. `maximum_concurrent_resolutions` defaults
to eight and accepts 1 through 64. `resolution_timeout_ms` defaults to 30
seconds and accepts 1 millisecond through five minutes.

The full validated `CredentialRef` is the singleflight key. The first waiter
creates an independent resolution task; concurrent waiters subscribe to that
flight. The task acquires one semaphore permit before entering
`spawn_blocking`, retains it until the backend settles, applies the exact
keyring/environment precedence once, removes the flight, and publishes one
cloned redacted result. Settled results are not cached.

The deadline belongs to each waiter. A timeout returns the non-secret account
identity and detaches only that waiter. It does not cancel the shared task or
recycle its permit before an arbitrary synchronous backend has returned.
Administrative set/unset operations remain a separate privileged contract and
are not part of resolution singleflight.

AI Language and Image routers call the same Credentials Resolve Local service,
so both capabilities inherit this boundary without duplicating scheduling.
Per-call Media resolution remains owned by `PrepareContext`, while HTTP body
and replacement limits remain owned by transport.

## Alternatives considered

Putting admission in each AI router was rejected because it would duplicate
policy, fail to protect non-AI credential consumers, and allow Language and
Image to bypass each other's limits. Restoring an AI Registry was rejected
because exact router/provider plugins already participate directly in Meta
generation ownership.

Caching resolved secrets was rejected because source precedence, rotation, and
secret lifetime belong to the credential backend. Force-cancelling a blocking
store call on waiter timeout was rejected because safe Rust cannot prove that
arbitrary synchronous backend code has stopped using its inputs.

## Consequences

At most the configured number of keyring reads execute concurrently per local
credentials generation. Identical concurrent references share work, but a new
call after settlement reads the current source again.

A hung backend can retain one admission slot after every waiter times out. The
remaining slots and caller deadlines keep the async service bounded; only the
backend or process can release the stuck synchronous work safely.
