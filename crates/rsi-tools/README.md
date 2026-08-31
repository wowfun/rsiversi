# rsi-tools

`rsi-tools` owns trusted process-local Tool catalogs and execution. The
[`rsi-tools-protocol`](protocol/README.md) package defines bounded schemas,
calls, canonical values, model-facing text/image content, and the catalog
interfaces. The ordinary [`rsi-tools`](core/README.md) plugin owns provider-wide
admission and retained results.

Contributors register only into an unpublished catalog stage. Registration is
atomic by batch and reversible while the stage is open. Sealing consumes the
stage and returns one immutable Tool Runtime: definitions, preparation, start,
query, wait, and commit then observe the same exact-name authority snapshot.
Late registration is rejected rather than changing a published Agent
generation. Registration leases can withdraw only from an open stage; dropping
or retiring one after sealing cannot mutate the published catalog or its calls.
Dropping an unsealed stage publishes nothing.

Retained-result capacity and shutdown are provider-wide rather than multiplied
by the number of immutable catalogs. A runtime generation pins exact executors
for admitted calls; dropping the caller waiter cannot abandon their eventual
result. Catalog lifetime is owned by its Agent generation, while provider
shutdown cancels and joins all admitted work under one bound. Contributor
leases do not own call cancellation or settlement after publication. Dropping
the immutable catalog cancels its admitted calls and reclaims both settled and
later-settling retained results for that exact catalog; provider-wide capacity
cannot outlive the authority that could commit it.
Policy, Approval, Jobs, Agent durable facts, and Code Mode are separate plugins
or consumers rather than registry internals. The orchestrator may pass one
typed, turn-scoped Jobs authority through `ToolStart`; this explicit invocation
capability does not make the Tool catalog own Jobs lifecycle or lookup.

Every prepared invocation carries an orchestrator-owned invocation identity in
addition to the model-produced call ID. Retained results are keyed by the Tool
catalog generation, invocation identity, call ID, and canonical request
digest, so equal provider call IDs in independent Agent turns cannot alias.
