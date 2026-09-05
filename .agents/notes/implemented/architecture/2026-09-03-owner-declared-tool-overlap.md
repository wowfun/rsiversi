---
name: Owner-declared bounded Tool overlap
comment: Permit concurrency without inferring effect independence from model output
---

## Problem

This note owns the Tool-definition scheduling assertion and its deterministic
publication consequences. Generic claim, durability, and checkpoint ownership
remain in the
[Session Kernel note](2026-08-26-durable-session-kernel.md); the executable
contract lives in the
[executor package](../../../../crates/rsi-agent/executor/README.md).

Executing every Tool call sequentially prevents independent read-only or
control-plane work from overlapping. Executing arbitrary calls concurrently is
not sound: model order may encode a dependency, most Tools can affect external
state, and completion order must not reorder durable Facts or later model
context. The executor cannot infer independence from a Tool name, arguments, or
result shape.

The scheduling decision must therefore live with the Tool definition owner while
the Kernel continues to validate durable effect ordering and the executor retains
one bounded lifecycle for cancellation, settlement, and cleanup.

## Decision

`ToolDefinition` carries a process-local `ToolScheduling` declaration.
`Exclusive` is the default, including after serialization, and
`ParallelSafe` is an explicit owner assertion. The declaration is not a model
argument and does not cross a durable or external seam as ambient authority.

For one model response, the executor overlaps only a contiguous source-order run
whose definitions are all `ParallelSafe`. An `Exclusive` call is a barrier: all
earlier work settles before it starts, and later calls wait for it. Intent Facts
retain the scheduling proof used for that execution, the Kernel rejects mixed or
undeclared active Tool effects, and result Facts are published in original call
order regardless of settlement order. A failed sibling does not erase evidence
from successful siblings: their results are published in source order before the
executor propagates the first failure.

Each call still follows prepare, durable intent, durable start, invoke, durable
result, and commit ordering. Within one parallel-safe run, all source-ordered
intents form one publication and durability barrier, followed by all matching
source-ordered starts and one durability barrier before invocation. The batch
does not weaken an individual start proof: every intent is durable before any
start is published, and every start is durable before any Tool runs. Results
remain individually published and committed because one sibling's generated
budget failure must not erase later settled siblings. Cancellation and retained
Tool settlement keep their existing admission ownership. Typed Tool execution extensions may carry the
orchestrator-owned caller capability without making it part of the Tool's
model-visible interface. The executor injects lane-parking authority only for
`ExclusiveFinal`; ordinary exclusive and parallel-safe calls cannot observe it.

`wait_agent` declares `ExclusiveFinal` and must be the last source-order call in
its response. This owner-declared scheduling variant is an ordering barrier and
cannot join a parallel batch. Its implementation can durably park and
temporarily return the executor lane; allowing later sibling calls would make
the Step's continuation depend on when a parked wait resumes.
Reacquisition is bounded by the Tool execution cancellation token. Cancellation
while parked returns a typed cancelled result instead of retaining the Turn's
claim indefinitely; ordinary completion publishes a Tool result only after the
lane has been reacquired.

## Alternatives considered

Keeping all Tools sequential was rejected because it leaves owner-proven
independent operations unable to use available execution capacity. Inferring
safety from known names, read-looking arguments, Sandbox mode, or observed
effects was rejected because those properties do not establish semantic
independence and can change behind the Tool interface.

Starting every call in a response concurrently was rejected because exclusive
effects would cross dependencies and barriers. Publishing results in settlement
order was rejected because timing would change durable history and model-visible
context. A dependency graph supplied by the model was rejected because it would
turn untrusted output into scheduling authority and enlarge the interface before
a demonstrated need.

Serializing `ParallelSafe` on the provider-facing Tool definition was rejected.
Scheduling is a local execution property selected by the installed owner, not a
capability a remote provider may preserve, alter, or invent.

## Consequences

Concurrency is opt-in and may be absent when no installed Tool owner makes the
assertion. Tool authors must use `ParallelSafe` only when overlap with adjacent
similarly declared calls preserves the same externally observable meaning.
Exclusive remains the safe behavior for old, decoded, or uncertain definitions.

Parallel calls may settle out of order internally, but callers, durable Facts,
and subsequent provider requests observe source order. One slow call delays
publication of later results in its run, and a failed call still waits for the
complete batch so successful side effects can receive durable results. Avoiding
that deterministic barrier would require a different model-context ordering
contract rather than an executor-only optimization.
