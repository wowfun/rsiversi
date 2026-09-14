---
name: Human continuation of an idle child Session
---

## Problem

The [recoverable tree decision](../feature/2026-09-03-recoverable-subagent-tree.md)
makes children durable and continuable. Terminal navigation can attach to a durable child and submit a human message,
but Kernel previously accepted that message and then repeatedly refused to claim
it after the parent activation settled. Persistent lineage does not imply a
currently executing parent. A real PTY child-send test exposes the stuck mailbox.

## Decision

Human input and independently admitted Goal continuations may claim an idle
child without an active parent. A parent activation that does exist still enters
the same atomic guard; Agent-sourced input still requires it. The immutable tree
path and root identity, tree lane, parent mailbox capacity and completion
reservation are unchanged. This is a narrow admission rule, not a second child
Session type or an implicit action performed by attach.

## Alternatives considered

Forking another root would break the requested same-Session continuation and
retained history. Starting the parent merely to unblock the child spends model
work unrelated to the human's input. Removing parent guards for Agent messages
would weaken existing control-Tool supervision.

## Consequences

Real local and daemon PTY scenarios attach an idle child, restore both drafts,
perform read-only tree usage, submit only on Enter and return to the parent.
Kernel tests retain active-parent guards and reject expired Agent callers after
the parent ends. Goal admission retains its existing durable source proof.


Finishing a manually continued child uses its normal completion reservation and
can wake the parent. Completion remains bounded and observable; navigation alone
cannot trigger that behavior. No Store or wire format change is required.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/kernel/README.md).
