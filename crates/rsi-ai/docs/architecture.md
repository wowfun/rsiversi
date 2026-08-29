# rsi-ai architecture

## Ownership

`rsi-ai-protocol` owns provider-neutral Language and Image meaning, including
exact model identity, validated requests, normalized streams, prepared-call
snapshots, dispatch evidence, and finite retry facts. `rsi-ai-provider` owns
provider-author adapter seams. `rsi-ai` and `rsi-ai-image` own the independent
Language and Image routers. Concrete provider plugins translate to their
private wire through `rsi-ai-transport` and register exact deployments with one
or both routers.

Credential resolution belongs to `rsi-credentials`; media bytes belong to
`rsi-media`; Tool schemas belong to `rsi-tools-protocol`. There is no AI-owned
credential manager, Meta bridge, ambient provider registry, model alias, or
fallback route.

## Ordinary plugins

Routers publish `LanguageCallContract` and `ImageCallContract` as Local
services. Provider plugins require the registrar contracts they enable and the
Base contracts needed by their adapters. Their registrations are bound to the
provider Fiber generation. Removing or replacing that Fiber withdraws the
registrations after generation-local prepared calls drain.

This topology uses the public `rsi-meta` Context/Fiber dependency semantics;
provider packages do not invent a parallel lifecycle or serialize in-process
calls through a private service bridge.

## Prepare and Start

Every call has an explicit two-stage effect boundary:

```text
describe(exact model) -> generation-pinned capability facts
compatibility(validated request) -> provider support check, no external I/O
prepare(validated request) -> redacted snapshot, no provider I/O
durable intent/start commit when required
start(consuming prepared value) -> exactly one provider attempt
```

The router owns the snapshot and rejects a provider adapter that changes it
during Prepare or in a later deferred checkpoint/batch. The snapshot freezes deployment, provider family, protocol, transport,
endpoint fingerprint, config generation, redacted credential source, canonical
request digest, and bounded retry policy. A provider attempt never retries
itself. A durable orchestrator may retry only after persisting failure evidence,
only when policy admits the error kind, and only when dispatch is proven not to
have crossed the effect seam. `Unknown` dispatch is never retried blindly.
Routers run the selected adapter's compatibility check before credential or
media-service access, then freeze those dependencies only for an admitted
request.

Provider-managed deferred Language work is separate. Submit, poll, resume, and
cancel each perform at most one request. Each resumed batch pairs normalized
events with the monotonic checkpoint that follows them, allowing an owner to
commit both atomically. A remote terminal status and a consumed terminal event
cursor are independent facts: polling may observe completion before any output
event has been resumed, so a checkpoint remains resumable until its event
stream terminal has been durably paired with a batch. Provider-private parser
state is a versioned durable format: a semantic layout change increments its
namespace-local version, and restore rejects every other version rather than
guessing a migration.
Restore also requires every frozen route fact, including retry policy, to match
the active provider generation. It intentionally supplies no submission-time
Media resolver: the remote operation has already consumed its original request,
and a checkpoint contains neither media descriptors nor authority to reconstruct
that request.

## Streams and media

Language output is indexed start/delta/end content followed by exactly one
Finished or Failed event. Image output uses bounded ordered chunks and a
terminal event. Assemblers enforce sequence grammar, total bounds, and terminal
presence; transport EOF is not success. Raw provider streams are normalized but
are not a success boundary until the owning caller finishes the corresponding
assembler. A durable owner that persists deferred checkpoints must bind each
checkpoint to its exact prepared-call snapshot and route generation.

Requests carry locator-free media descriptors for untrusted provider input.
Routers sum unique declared media bytes and reject a request above the 256 MiB
resident budget during Prepare, before credential, Media, or adapter I/O.
Start-time resolution acquires the prepared call's complete request weight in
one operation, then verifies the declared length and SHA-256 before provider
translation. It never retains one descriptor's budget while waiting for the
rest of the same call. Pending admission is single-flight only while at least
one descriptor waiter owns it; cancelling the last waiter withdraws that queue
position, while successful admission follows the resolved-media cache. Provider URL
fetching is not part of the contract. Durable `MediaRef` values have the
canonical PNG MIME owned by `rsi-media`; descriptors and refs are different
roles rather than interchangeable MIME claims. Tool execution and durable
session projection belong to their own packages.
