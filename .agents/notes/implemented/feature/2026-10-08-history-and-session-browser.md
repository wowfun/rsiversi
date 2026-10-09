---
name: Cross-workspace history reuse
---

## Problem

Conversation-scoped text search requires users to know the source identity.

## Decision

History discovers finite locally saved sources under current caller authority.
Models retain their workspace boundary; humans explicitly transfer selected
original data through independently admitted source and target scopes.
Search pagination expires when content or visibility changes.

## Alternatives considered

Full corpus snapshots require retaining per-source horizons and versions without
solving authorization revocation. Conservative cursor invalidation is simpler.

## Consequences

History pagination expires on any index revision or admitted-source-set change.
An empty indexing batch with unchanged coverage publishes no new revision. Polling
an already indexed source therefore preserves a page without retaining a snapshot.
The discovery pass is finite and live rather than an atomic Host snapshot;
partial metadata failure is explicit and a fresh pass retries it. This avoids
retaining corpus snapshots while preserving current authorization.

History range work retains a permit per execution location, rather than per
conversation. Keeping every source's separate admission alive through a query
exhausted the standard operation pool at 64 saved conversations, including the
extra permission check required before publication. Sharing the retained pin
preserves actual-settlement ownership without treating catalog entries as
simultaneous backend operations; fresh source and final checks remain separate.

Scope, limits and lifecycle contracts live with
[History](../../../../crates/rsi/history-api/README.md),
[References](../../../../crates/rsi-agent/references/README.md). Default validation stays
keyless and isolated; confined native, provider and visual probes remain opt-in.
