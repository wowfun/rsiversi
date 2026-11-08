---
name: Trust immutable tool definitions after admission
---

## Problem

Tool definitions already reject invalid names, descriptions, schema shapes,
JSON bounds and freeform grammars at construction or deserialization. Their
private fields and borrowed accessors prevent safe Rust consumers from changing
those values. Catalog registration and Language request construction nevertheless
repeated the Tools validation, including allocating a complete encoded schema.
Attaching a validated freeform grammar also repeated the unrelated schema work.

## Decision

The [Tools protocol](../../../../crates/rsi-tools/protocol/README.md) owns this
construction invariant. It exposes no revalidation method for either immutable
definition, and attaching a freeform grammar is infallible. Registration and
Language admission validate only their own additional constraints. No cache,
new retained payload or durable format is needed.

DSH's `packages/core/tools/src/schema.ts` compiles parameter and output schemas
inside `defineTool`; its `ToolsService.resolve` selects definition references
instead of recompiling their schemas for each resolved view. RSI applies that
reuse principle to its stronger closed Rust values. External JSON still enters
through the validating decoder; model arguments retain their own validation.

## Alternatives considered

Keeping public revalidation or returning an always-successful result would leave
an unnecessary API and encourage repeated work. Caching encoded schemas would
retain redundant data where removing the duplicate operation is sufficient.
Removing Language's narrower name, JSON structure or aggregate bounds would
change capability admission and is unrelated to this simplification.

## Consequences

This pre-release source API changes: `with_freeform` returns the definition
directly, and immutable definitions have no public `validate` method. Wire
fields and accepted boundary values remain unchanged. Construction and decoding
tests own the invariant, while registry and Language tests retain their added
relationship and aggregate limits.
