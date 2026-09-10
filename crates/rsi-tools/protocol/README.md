# rsi-tools-protocol

Execution-policy paths use the bounded
[host-path grammar](../../rsi-workspace/path/README.md) when validated as data.
Native process and sandbox providers retain actual filesystem validation.

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
`ToolExecution::workspace_read()` asks that same pinned Sandbox generation for
an immutable workspace read scope using the resolved policy. It accepts no model
paths or mode overrides, rejects already-cancelled execution, and adds no process
stamp to the enforcement collector. It changes neither Tool admission nor approval.
Its opaque typed extension map may also carry a Tool-layer lane-parking
authority. A blocking orchestration Tool explicitly parks before waiting and
must reacquire the same bounded executor admission before returning a result.
Reacquisition observes the Tool execution's cooperative cancellation: a
cancelled wait may return `Cancelled` without admission, while no successful or
model-visible error result may cross the parking boundary until admission is
held again. Model arguments cannot forge or discover that authority.

The caller supplies one bounded invocation identity when preparing a call.
Durable orchestrators use their own effect identity; the Tool layer does not
guess session or turn structure from a model-produced call ID.
Every definition also carries process-local scheduling metadata. `exclusive`
is the default; only a Tool whose owner explicitly marks `parallel_safe` may
overlap adjacent calls from one model response. This metadata is not serialized
to providers and therefore cannot be asserted by model output. An
`exclusive_final` Tool is also an ordering barrier and additionally requires
the call to be last in provider source order; this is the scheduling contract
for a Tool that may park its executor lane while it waits.

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
execution registration timeout is within
1..=600,000 milliseconds. The owner-declared `HumanInteraction` policy instead
requires `exclusive_final` scheduling and ends on answer or cancellation; it
has no implicit answer or timer. The current
pre-release result shape is exactly `{ value, content, is_error }`; image
content contains a durable `MediaRef`, not an inline blob or status envelope.
Model-facing text rejects C0 terminal controls other than tab and line breaks.
`safe_tool_text` decodes UTF-8 lossily and replaces exactly those disallowed
controls; byte-source Tools retain exact bytes separately and identify altered
display text. This helper neither archives output nor grants content trust.
The removed v0 status/blob tool-envelope shape is not forward-compatible and
has no migration reader.


## Portable contributions

`portable` owns the version-1 `rsi.tools.portable` duplex byte protocol. The linked
bridge requires one explicitly named Portable supply and the exact Local
ToolRegistrar. Describe returns a nonempty atomic batch of at most 64 bounded
definitions with owner-declared finite timeout and scheduling. Describe contains
no registration callback or authority; the bridge registers the entire validated
batch through the ordinary unpublished stage. Sealing and generation ownership
retain their existing meaning.

Each Execute sends one validated ToolCall and its orchestrator-pinned execution
policy. The provider may request Confine with only program and argv; the bridge
uses that invocation's existing ToolExecution::confine, preserving Sandbox
identity, mode, cwd/workspace and the host-owned enforcement collector. The reply
contains the actual process plan, with lossless platform-tagged OS strings.
Mismatched platform strings fail closed. Process spawn remains trusted native
code; this protocol is not a sandbox around the plugin itself. It transfers no
Jobs scope, extension map, credentials, media bytes, or arbitrary Local service.

One Result closes the exchange; success requires exact clean Portable terminal.
Results may not assert enforcement stamps; the existing Tool runtime attaches
host-collected evidence. Approval and model policy run before this executor;
neither Describe nor Execute offers an approval bypass. A pre-cancelled call
never opens a Portable call. Cancellation cancels and drains the existing Meta
call; native callback failure retention remains the Native Loader's authority.
No bridge response establishes that a failed foreign callback has ceased work.

Each JSON frame uses the canonical Tool JSON decoder (including duplicate-key,
depth, node and exact-number checks) and is bounded to 256 KiB before decode or
during encode. Tool content variants reject unknown fields. Each
execution admits at most 256 Confine requests. These are this wire's limits;
linked Tool values retain their own wider bounds. Oversize, unknown fields,
extra messages/capabilities, wrong variants and missing terminal fail closed.
