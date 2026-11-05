---
name: Return Tool refusals to their caller without ending the Turn
---

## Problem

The executor records an exact Tool rejection, then treats both policy denial and
declined approval as fatal Turn failures. Context already folds model-origin
rejections into error replies, but the executor never lets the model consume
that reply in the same Turn. This prevents an explanation or an alternative
approach and makes wrapping native Tools with application hooks insufficient.

DSH's Tool pipeline materializes denial as a post-execution error-shaped result
without dispatching the body. The policy still refuses the operation; its caller
continues. That behavior fits RSI's existing durable rejection and outcome fold.

## Decision

The [executor](../../../../crates/rsi-agent/executor/README.md) distinguishes
prepared execution from a staged refusal. A refusal settles its call, never
starts the body, and participates in source-ordered batch publication. Parallel
admission must not publish a later refusal before an earlier executing result:
the Context outcome-order invariant remains intact.
Kernel permits a source-proven refusal while a sibling effect is active because
the refusal starts no execution. It retains exact unconsumed-call proof and all
ordinary effect-start and model-request gates.
The existing [Context fold](../../../../crates/rsi-agent/context/README.md)
provides its model-visible reply. Program callers receive the corresponding
error result and decide how their coordinator handles it. No second result Fact,
protocol variant, durable format or execution owner is needed.

The application projection also records DSH `tool/call` before the error result
of a refused or skipped model call. DSH's batch scheduler emits this event before
pre-execute policy and for skipped calls; it means invocation, not body dispatch.
The missing event previously hid refusal details in the original cold UI even
though the result was present. Native `ToolStarted` remains absent. A stored
application projection with the old source prefix cannot be silently reindexed:
its original local event positions and references remain authoritative.

## Alternatives considered

Ending every Turn after refusal preserves the existing control flow but denies
the caller an ordinary recovery path. Pretending the body ran and recording a
ToolResult would erase the distinction between rejected and executed work.
Reinvoking the rejected call or changing its arguments would cross the exact
admission boundary; continuing never does either automatically.

## Consequences

Policy and approval decisions remain binding for every individual invocation.
Parallel siblings that were admitted may execute; rejections and real results
reach PostTool callbacks in model source order. A model can request a different
operation, which crosses ordinary preparation and policy again. Explicit user
stops and framework failures still stop the drive. Tests must prove the original
body remains uncalled, the rejection is durable, the next request contains its
reply, and mixed batches retain call/result pairing and order.
