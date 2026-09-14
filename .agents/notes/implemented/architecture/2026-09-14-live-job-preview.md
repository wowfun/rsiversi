---
name: Claim-bound non-consuming process preview
---

## Problem

`Jobs::read` reports terminal jobs and releases captured output. Reusing it for
presentation changes finalization semantics. Direct foreground Bash bypasses
Jobs, so a UI cannot discover its output through the claim-bound Jobs relay.

## Decision

Process owns a bounded raw tail peek. Jobs delegates a synchronous peek without
reader leases, reporting, waiting, cancellation or scope acquisition. A submitted
job carries an optional opaque originating invocation identity. The Agent executor
injects its durable Tool effect into that trusted extension; Bash forwards it.
Agent foreground and background Bash share producer admission. Standalone calls
without a Jobs scope retain direct Process execution.

The Kernel validates Session, Header, Turn, claim generation and origin on both
sides of a read. The Session API exposes a required read-only operation with at
most 64 KiB encoded output, 32 KiB per stream and a combined base64 byte budget.
A focused terminal preview polls at most four times per second with one pending
read; closing it cancels that task. Missing live output falls back to durable Tool
results and their best-effort completed-output references.

## Alternatives considered

Polling `Jobs::read` changes reporting. Keeping a ManagedProcess in presentation
extends ownership beyond its scope. Reconstructing a scope from its textual name
can attach to a replacement generation. None preserves the existing lifecycle.

## Consequences

Native Linux tests prove bounded tails, unchanged reporting and finalization,
foreground output before completion, cancellation on scope retirement, rejected
foreign/stale identities, and no polling after the visible preview closes.


Output is process-local and may disappear between two reads. A read never grants
durability or reactivation. Producer callbacks remain trusted synchronous code;
the Jobs provider contains panics and validates returned byte/offset bounds.

The API client enforces its generation-local four-Hertz budget; the current API
context does not identify individual server connections. Server-wide worker
admission still bounds concurrent peeks. The terminal explicitly opens a focused
output detail and polls only while it stays open; it does not subscribe to every
Tool card or child Session.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/turn-protocol/src/job_preview.rs).
