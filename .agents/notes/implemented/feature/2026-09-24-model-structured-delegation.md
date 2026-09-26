---
name: Bounded model-authored structured delegation
---

## Problem

Delegation without a model-facing output contract cannot reliably return a
validated machine-readable value. A reader placeholder also prevents Context from
using the result even when the programmatic reader verifies it.

## Decision

This partially supersedes the schema-authority and standard reader defaults in the
[subagent decision](../../implemented/feature/2026-09-03-recoverable-subagent-tree.md).
The model spawn adapter validates an 8 KiB annotation-free schema using the
existing OutputContract. The standard reader returns bounded encoded pages,
while programmatic access retains full values and exact initial Completion checks.
Schema choice grants no additional Tool or Sandbox authority. Schema data can
still influence model behavior; annotation rejection is not injection isolation.

## Alternatives considered

Keeping only a programmatic producer cannot serve ordinary model delegation.
Putting the full 256 KiB value into every read needlessly consumes parent context.
Silent truncation would misrepresent incomplete JSON as a usable result.

## Consequences

The standard catalog changes intentionally. Page sizes bound encoded presentation,
not total context across repeated reads or process RSS. Only the initial child
activation can produce the contracted result; later activations cannot replace it.

The parent provider request contains an exact bounded result page. Pagination
round-trips UTF-8 and escaping, schema metadata is rejected without rejecting
property names, and old locator/admission/retry checks remain covered.
