---
name: Fetch registry evidence for each independently locked workspace
---

## Problem

Standalone fixture locks can contain crates absent from the root lock. An audit
with fetching disabled after checking only the root cannot establish yank status
for those additional crates and fails with missing registry-index entries.

## Decision

CI audits every tracked Cargo lockfile with registry fetching enabled and collects
all failures. Each audit may refresh advisory evidence as well. The workflow owns
the command sequence; its isolated script tests cover tracked-file selection,
paths with spaces and failure aggregation.

## Alternatives considered

Fetching once from the root is insufficient for independently resolved locks.
Disabling yank checks would discard part of the audit. Combining dependency graphs
would change the workspaces being audited.

## Consequences

Audits depend on live registry and advisory availability for each workspace.
They retain the existing online CI boundary and require no local credentials.
