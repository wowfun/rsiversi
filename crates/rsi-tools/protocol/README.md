# rsi-tools-protocol

This package owns bounded process-local tool definitions, schemas, calls,
results, and catalog interfaces. Canonical JSON values are distinct
from ordered model-facing text and Media references. Every typed JSON value is
bounded by encoded bytes, nesting depth, and node count; callers importing a
Tool definition into a narrower capability must revalidate that capability's
own limits. Model-produced numbers are accepted only when their exact decimal
value survives the canonical `serde_json::Value` representation. Serializable
calls and execution policies revalidate those invariants during
deserialization rather than relying on a later runtime consumer.

It contains no registry implementation, policy, approval, durable logging,
provider wire, or plugin lifecycle. Tool start carries the exact sandbox
planner and an optional typed Jobs scope supplied by the orchestrator; these
are invocation authorities, not registry-owned services or ambient lookups.

The caller supplies one bounded invocation identity when preparing a call.
Durable orchestrators use their own effect identity; the Tool layer does not
guess session or turn structure from a model-produced call ID.

A catalog stage admits at most 64 Tool registrations. `ToolRegistrar` is the
write-only staging interface; `ToolRuntime` is the immutable execution
interface; `ToolCatalogProvider` creates bounded stages whose `seal` operation
publishes no partial state. A registration lease withdraws its exact batch only
while that stage remains open. After sealing, releasing or retiring the lease
is a no-op: the immutable catalog owns its executors and calls until catalog or
provider teardown. Retained identities are valid only for that catalog
lifetime. Dropping it cancels active calls and reclaims settled capacity
immediately; an active body keeps its provider-wide invocation admission until
true settlement, and an outcome produced after catalog withdrawal is
discarded. Starting above the provider-wide admission bound returns
`ToolError::Capacity`; starts after shutdown begins return
`ToolError::ShuttingDown`. Each
registration timeout is within
1..=600,000 milliseconds. The current
pre-release result shape is exactly `{ value, content, is_error }`; image
content contains a durable `MediaRef`, not an inline blob or status envelope.
Model-facing text rejects C0 terminal controls other than tab and line breaks.
The removed v0 status/blob tool-envelope shape is not forward-compatible and
has no migration reader.
