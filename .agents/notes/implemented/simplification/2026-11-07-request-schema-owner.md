---
name: Model request evidence owns durable tool schemas
---

## Problem

Application request capture retained each complete Tool definition beside the
sealed runtime and copied it into every `StepPrepared`. Model request evidence
also saved the actual provider-neutral definitions, with direct references for
unchanged sections. Registration routing and cold result recovery read only the
tool name, owner and registration occurrence from the preparation. The second
schema catalog increased memory, serialization, history reads and durable bytes
without contributing to these decisions.

## Decision

The [Session contract](../../../../crates/rsi-agent/session-protocol/README.md)
separates registration provenance from model-request inspection. The captured
runtime owns executable definitions. Preparation retains names and occurrence
identities; the existing request evidence owns durable model schemas. Capture
visits borrowed definitions instead of cloning a temporary schema catalog.

DSH's `ReactLoopAgent.buildRequest` records its resolved tools in `request/header` and
compares headers before publishing another observation. It does not persist a
second schema catalog for native result recovery. RSI adopts that ownership
separation while retaining its existing bounded evidence references and native
receipt identities. Neither source revision nor a new schema registry is needed.

## Alternatives considered

Interning complete preparation views would retain another resolver or cache and
would miss repeats when occurrence identities change between captures. Replacing
the existing evidence reference mechanism would add complexity without changing
which data must be retained. Keeping both catalogs solely as a fallback would
continue paying for repeated definitions in every normal request.

## Consequences

Provider retries keep the exact captured executable catalog. Later request series
capture fresh registrations, and cold recovery still authenticates the original
owner and registration without invoking tools again. Request evidence continues
to describe the actual model tools, including preset tools and selection policy.

When evidence exceeds its existing budget, offline schema inspection is unavailable.
This does not block native execution or recovery. The format change is explicit:
older Session headers are unsupported, without rewriting immutable history or
introducing a legacy decoder. The SQLite table layout is unchanged. This removes
one repeated catalog; it does not establish whole-application memory suitability
for small devices or remove accepted-versus-claimed message duplication.
