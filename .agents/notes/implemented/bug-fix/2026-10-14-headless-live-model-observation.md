---
name: Observe live models during one-shot terminal messages
---

## Problem

One-shot terminal messages start an interaction observer after their Turn is
claimed, but never start the new independent model observer. Streaming providers
produce no answer until settlement, despite the renderer already supporting live
prefixes. The undrained-stdout regression reproduces this with an unfinished
200000-byte provider response; only cancellation reveals the settled output.

## Decision

The [terminal contract](../../../../apps/terminal/README.md) observes both transient
models and interactions for each claimed message. Both observations retire and
join before the terminal outcome enters the renderer, preserving its final-event
ordering. Durable Facts remain owned by the message driver.

## Alternatives considered

Waiting for a completed provider response hides the missing live path. Adding
per-delta durable Facts would undo atomic attempt settlement and expand history.

## Consequences

Text and JSONL regression cases require actual model bytes in the pipe before
SIGINT, for both unfinished and completed provider streams. Existing text cursor
deduplication emits only the remaining suffix from the settled attempt. Cleanup
retains the enclosing application task tracker and output cancellation owner.
