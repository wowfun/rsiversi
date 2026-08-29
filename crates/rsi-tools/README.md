# rsi-tools

`rsi-tools` owns trusted process-local tool registration and execution. The
[`rsi-tools-protocol`](protocol/README.md) package defines bounded schemas,
calls, canonical values, model-facing text/image content, and executor seams.
The ordinary [`rsi-tools`](core/README.md) plugin owns the exact-name registry.

A registration belongs to its lease. Schema projection and dispatch snapshot
the same active definition, so a hidden or withdrawn tool cannot still be
invoked through another path. Policy, Approval, Jobs, Agent durable facts, and
Code Mode are separate plugins or consumers rather than registry internals.

Every prepared invocation carries an orchestrator-owned invocation identity in
addition to the model-produced call ID. Retained results are keyed by the Tool
registration generation, invocation identity, call ID, and canonical request
digest, so equal provider call IDs in independent Agent turns cannot alias.
