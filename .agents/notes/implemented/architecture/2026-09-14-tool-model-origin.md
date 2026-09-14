---
name: Tool-bound model origin for child Sessions
---

## Problem

A claim identifies a Turn, which can contain several requests and concurrent
Session selection changes. It cannot identify the settings that produced a
particular Agent control Tool call.

## Decision

ToolIntent names its producing model effect. Kernel authenticates a completed
Conversation in the same Turn and the exact call ID, name and decoded arguments.
Only then may it issue a Tool-bound caller with the producing request's actual
model and effective effort. Child creation freezes those settings; an explicit
child model replaces the route and resets inherited effort unless supplied.
This narrowly supersedes route inheritance and spawn admission in the
[recoverable tree decision](../feature/2026-09-03-recoverable-subagent-tree.md).
Claim-only authority remains for internal domain and supervision operations,
but cannot create children. Tool authority expires when its effect settles.

Kernel retains only the latest request's bounded Tool call proofs, never its
ordinary prose. Argument fragments use shared bounded chunks so speculative
publication does not repeatedly copy a growing full argument string. Completed
call arguments become fixed SHA-256 fingerprints of typed JSON equality. They
are consumed by exact ToolIntent admission and recomputed during recovery,
so parsed JSON trees do not accumulate in resident control state.

## Alternatives considered

Reading the current selection races with delayed Tools. Reading the parent
Header ignores Session and explicit Turn selection. Trusting an Executor-supplied
model without checking its source weakens the durable admission boundary.

## Consequences

Tests reject wrong effect, purpose, call, arguments, unfinished source and stale
Tool authority; prove source settings survive later selection changes and
explicit child overrides; and exercise parallel calls and recovered histories.


Source proofs add bounded live control memory and durable replay work. Calls are
limited by AI block/output and Tool argument bounds, and the Turn's generated
record budget remains authoritative. No old-format compatibility is provided.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/kernel/src/tool_origin.rs).
