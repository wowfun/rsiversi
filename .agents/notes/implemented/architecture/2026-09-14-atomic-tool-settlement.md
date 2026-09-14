---
name: Pure domain contributions at Tool result publication
---

## Problem

PostTool callbacks run after ToolResult durability. Updating authoritative state
there leaves a crash interval where the result is present but the state is not.
Calling Kernel directly from a Tool would create a second mutation authority
and complicate retained-result recovery.

## Decision

A synchronous, effect-free ToolSettlementContributor receives the exact
ToolIntent, retained ToolResult and bounded current domain snapshots. It returns
only typed proposals. Executor submits these proposals and the ToolResult in
one existing DomainMutation, whose AtomicAgentCommit publishes both streams.
Normal, parallel and retained-result paths use the same publication function.
Callbacks run outside Kernel locks and receive neither a historical reader nor
mutation or external-I/O authority. Existing PostTool Goal and repeat-tool
contributions keep their later-stage semantics.

An exact pending Tool result can settle after cancellation while its live
mutation admission remains open; this records already completed work and does
not admit another external effect. Ending and exhausted-budget boundaries still
reject business commits. A lost acknowledgement retains the same domain request
identity and is reconciled from its receipt; it never reruns the Tool or callback.

## Alternatives considered

PostTool projection cannot provide atomic authoritative state. A Tool-owned
Kernel mutation capability duplicates submission and recovery rules. A separate
Todo event log creates competing state authorities and an additional durable
transaction model.

## Consequences

Injected failures before apply, after apply and during receipt lookup prove
all-or-nothing state/result visibility and no Tool or callback re-execution.
Normal, parallel and retained publication, cancellation, stale source, proposal
conflict and result-budget exhaustion exercise the same owning boundary.


An additional callback stage must remain bounded and pure. Domain conflicts
are explicit failures, never silently dropped Todo writes. Terminal admission
cannot be reopened by result settlement.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/executor/README.md).
