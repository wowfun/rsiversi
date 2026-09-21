---
name: Frozen tool output declarations
---

## Problem

Canonical tool values already survive Agent replay, but author declarations do
not identify their structure. Looking up the current plugin while replaying an
old result can silently apply a different interpretation.

## Decision

Declarations belong to the Tools registration contract. Agent composition freezes
the exact-name catalog in a domain with codec 1, seals Tools before capturing that
domain, then seals Domains. A restored typed generation must match the saved
catalog. Legacy sessions without that baseline remain readable as raw history;
they cannot silently acquire a new typed execution contract.

The [Tools protocol](../../../../crates/rsi-tools/protocol/README.md) owns
declaration limits and the Portable version 2 boundary. `ToolDefinition` and
`ToolResult` retain their wire shapes; declarations do not add per-result durable
fields or change Native ABI 3. The Session and Store owners independently version
their formats, including the selected-reference change. Typed author rendering consumes one canonical Rust
result. Presentation stays above Tools.

## Alternatives considered

Adding declaration fields to every ToolResult would break the closed durable
format and repeat schemas in history. Current-registry lookup cannot establish
historical identity. A general reference-resolving JSON schema engine would add
network and recursive evaluation authority unnecessary for these declarations.

## Consequences

Tests exercise declarations across registration, Portable import, scoped catalogs,
composition restore and a real tool's presentation. They verify atomic aggregate
admission, digest tampering, unsupported schemas, output mismatch, missing legacy
baseline and changed declarations without rewriting stored history.

Real Linux PTY, Chromium, Firefox and Linux desktop fixtures render persisted
typed results. The declaration supplies interpretation, not authorization to
execute a Tool or trust arbitrary presentation content.

Portable version 2 requires rebuilding contributors. A deliberately limited
schema dialect cannot directly import arbitrary external schemas; undeclared
tools remain opaque until their adapter provides a supported declaration.
