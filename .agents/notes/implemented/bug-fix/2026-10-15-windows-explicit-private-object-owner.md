---
name: Create preset staging and SQLite sidecars with an explicit Windows owner
---

## Problem

An elevated Windows token can create ordinary temporary directories and
SQLite sidecars with Administrators ownership. Private admission requires the
exact TokenUser owner and refuses to repair foreign ownership. CI exposes failed
preset materialization and failed second SQLite connections after the first write.
The descriptor fixture also incorrectly assumes TokenUser always has numeric SDDL
spelling; a built-in account can canonicalize to an alias.

## Decision

Preset staging uses native relative private creation with an explicit TokenUser
owner. SQLite acquires or creates WAL and shared-memory sidecars through the same
private native file seam before opening the connection. These contracts belong
to [composition](../../../../crates/rsi/core/README.md) and
[SQLite Storage](../../../../crates/rsi-storage/sqlite/README.md).
Existing foreign-owned objects are still refused; ownership is never repaired.
Descriptor tests compare the actual canonical owner with its corresponding ACE.

## Alternatives considered

Allowing Administrators as a substitute owner widens durable admission. Changing
CI to run without elevation hides a real supported-token execution path. Applying
ownership after creating an ordinary staging object introduces an avoidable
publication window.

## Consequences

Existing deterministic publication, independent SQLite connection and built-binary
Windows ACP targets exercise the actual affected consumers. Local ordinary-token
results and elevated CI results remain distinct platform evidence.
