---
name: Durable Session model and reasoning selection
---

## Problem

Client-local model overrides freeze ordinary queued requests and leave Goal
continuations and child creation using a different default. The closed effort
enum cannot represent adapter-specific choices, and ordinary Agent requests do
not apply language generation settings.

## Decision

An Agent-only model-selection domain owns a complete optional model/effort
selection. Each Step captures its current value before assembly; retries keep
that capture. Explicit whole-Turn overrides precede the domain, which precedes
the frozen Header baseline. Ordinary UI submissions omit overrides. Startup
default persistence remains an independent, explicit operation.

LanguageProfile declares bounded adapter-owned effort identifiers and its
default. Prepare validates each selection, and durable request snapshots retain
requested and effective choices. A child starts from the actual model request
that produced its spawn ToolIntent, not a later selection. The selection domain
resets on fork and falls back to that child's own frozen baseline.

This supersedes the client-local selection clauses of the [terminal application
decision](../feature/2026-09-06-terminal-application.md); setup, command receipts and uncertain-outcome reconciliation remain.

## Alternatives considered

Whole-Turn UI overrides cannot implement the selected DSH Web next-request
semantics. Forking on every model change loses same-session continuity. A
provider-neutral ordinal enum invents equivalences that adapters do not share.

## Consequences

Tests distinguish next-Step changes from retry stability, explicit overrides,
Goal continuation, cold recovery and delayed/parallel child creation. Every
adapter rejects unsupported effort before I/O. GUI and terminal submissions
consume the same Session choice.


This changes durable Header/Fact and provider wire formats. Old versions must
be rejected before writes. Provider reconfiguration can invalidate a saved
selection; failure must retain the choice and explain the unavailable route.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/model-selection/README.md).
