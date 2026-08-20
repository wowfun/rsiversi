---
name: Replayable agent turn runtime
comment: Product boundary and durable execution model for the first rsi-agent vertical slice
---

## Problem

`rsi-meta` composes and routes trusted native services but deliberately does not own model-visible history or agent semantics. Building model integration directly into that platform would mix composition authority with application policy. Conversely, starting with workflow graphs, self-modification, provider-specific clients, or a broad plugin framework would create large public seams before the fundamental model-tool loop and its failure semantics had evidence.

The first agent capability also needs a trustworthy answer after cancellation or process failure. An in-memory loop cannot prove which context a model saw or whether a tool may already have executed, and replaying effects to reconstruct state is unsafe.

## Decision

`rsi-agent` is a separate product above the public `rsi-meta`
`CompositionHost`. Its public runtime façade is the cloneable `AgentHost`;
coordination, persistence, service adapters, request derivation, and recovery stay
private. Provider-neutral AI semantics belong to `rsi-ai-protocol`, while
`rsi-agent-protocol` owns only the closed tool-service contract.

Each caller-supplied session executes one bounded turn in v0. A session is
bound to the exact model and prompt accepted first: identical callers may join
or replay its terminal result, while either field changing is a conflict. This
makes a caller-owned identifier an idempotency key without letting arrival
order silently change model selection.

The append-only SQLite transcript is the source of truth. Model-visible input
and tool dispatch intent are committed before their external effects, and only
a typed successful commit receipt may advance in-memory state. Accepted work
continues independently of the caller toward a durable terminal outcome. An
uncertain write or nonlocal store failure makes the host terminal until reopen;
corruption confined to one closed transcript remains local to that session.

The current implementation and invariant evidence are authoritative in the
[product architecture](../../../../crates/rsi-agent/docs/architecture.md) and
[testing contract](../../../../crates/rsi-agent/docs/testing.md); this note records
why those boundaries were selected rather than duplicating their mechanics.

## Alternatives considered

Putting the loop inside `rsi-meta` was rejected because composition must remain
usable without agent policy or transcript knowledge. Public model, tool,
persistence, and executor traits were rejected because v0 has no second
production implementation and those seams would export coordination complexity.

An in-memory transcript was rejected because exact request reconstruction,
idempotent reopen, and unknown tool outcomes are core semantics. Automatically
restarting incomplete calls was rejected because an unrecorded result does not
prove that a dispatched effect did not happen.

A global serial actor, unbounded task pools, and eager validation of all closed
history were rejected respectively for head-of-line blocking, unbounded
resource growth, and startup cost proportional to untouched history. Workflow
graphs, multi-agent orchestration, parallel tools, compaction, and multi-turn
continuation were deferred until the smaller durable loop has evidence.

## Consequences

The first product supports one turn per session, sequential tools, strict
resource limits, and a concrete private SQLite implementation. Native providers
remain trusted in-process code. Recovery closes incomplete work as interrupted
without invoking providers, favoring auditable uncertainty over automatic
re-execution.

Future adapters may implement the semantic service contracts without changing
transcript ownership. Multi-turn continuation, effectful parallel tools, or a
different durability backend require new evidence and an explicit boundary
decision; none is implied by the v0 façade.
