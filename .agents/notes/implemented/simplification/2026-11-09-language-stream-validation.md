---
name: Validate model evidence without assembling discarded output
---

## Problem

Complete model-attempt admission and incremental prefix decoding used the Language
assembler solely to check event grammar. Alongside the compact evidence they
retained another copy of text, reasoning, arguments, sources and warnings, and
computed the encoded weight of the assembled output before discarding it.

## Decision

The AI protocol owns one incremental stream validator. It keeps block states,
counters and terminal metadata, independent of payload length. The assembler
uses that validator and collects content in a contiguous vector. Durable attempt
and prefix consumers use the validator directly. The public semantic contract
is in the [protocol README](../../../../crates/rsi-ai/protocol/README.md).

DSH's `packages/core/session/src/index.ts` admits replay events by validating their
shape without reconstructing another complete model message. RSI adopts that
separation of admission from projection while preserving its own normalized
stream grammar and durable bounds.

## Alternatives considered

Skipping durable grammar checks would admit malformed evidence. Maintaining a
second lighter grammar would let admission diverge from live assembly. A second
cached decoded message would retain the duplication rather than remove it.

## Consequences

Validation memory grows with bounded block count rather than output size. Model
wire bytes and output semantics remain unchanged. Callers needing complete output
still pay for collecting it. This removes a specific avoidable cost; it does not
establish a whole-Host memory budget or session-search performance parity.
