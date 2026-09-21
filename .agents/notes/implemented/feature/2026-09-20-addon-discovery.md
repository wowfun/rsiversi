---
name: Snapshot-bound addon discovery
---

## Problem

Public addon descriptions exist, but callers cannot page through factory,
exported contract and exact-generation Tool metadata using one capture.

## Decision

The standard product captures its existing addon declarations and an optional
Agent pin. Bounded exact lookup and paging expose descriptions without factory
execution, configuration values or invocation capabilities. A cursor belongs
to one immutable capture, including when another capture has equal content.

## Alternatives considered

An AST inventory cannot establish runtime placement or nominal Local identity.
Reading live registries on every page can mix generations. Running prepare to
discover metadata executes author code and can resolve sensitive configuration.

## Consequences

Public-boundary tests use factories that fail if invoked during discovery. They
check exported contracts, absent keys, item and byte paging, complete traversal
and cross-capture cursor rejection. Tool definitions and outputs come from the
same scoped pin.

Descriptive schemas cannot prove successful activation. Local contract keys do
not imply a wire codec. A snapshot retains the supplied pin until its readers
release it, as other immutable generation consumers do.
