---
name: Host-native child environment identity
---

## Problem

Windows pipe and ConPTY launches selected different values when a typed child
environment contained names differing only by case. Exact-string uniqueness did
not establish Windows environment uniqueness.

## Decision

The [Process boundary](../../../../crates/rsi-process/core/README.md) rejects
Windows-equivalent duplicate names before provider admission. A names-only
standard-library Command environment map performs the host's ordinal comparison;
it starts no process and reads no ambient values. POSIX keeps exact native names.

## Alternatives considered

ASCII folding misses Unicode aliases. Unicode string uppercasing is not Windows
ordinal comparison and cannot preserve arbitrary native UTF-16. A second syscall
wrapper would duplicate the standard library's existing safe process semantics.
Choosing a duplicate value would silently change the caller's requested plan.

## Consequences

Every Process mode shares the rejection. The temporary map is bounded by the
existing environment count and byte ceilings and retains names with empty values.
