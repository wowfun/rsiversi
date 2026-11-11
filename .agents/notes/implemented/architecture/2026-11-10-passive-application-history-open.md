---
name: Open persisted application history without Agent activation
---

## Problem

DSH history opening called the native live projection stream just to obtain
model selection. That stream rebuilt all Agent contributions; a failed MCP
restoration blocked the history page. The companion already ignores this field
for durable Sessions and reconstructs selection from the immutable model baseline
and canonical controls. DSH's own HistoryController uses independent Session
observations without activating an Agent.

## Decision

Use the existing authorized durable summary for every persisted history open.
Only unpublished drafts use the live projection path. This replaces the extra
activation with the same summary boundary already used by captured history.
The current contract belongs to the [Serve README](../../../../apps/serve/README.md).

## Alternatives considered

Making failed MCP restoration return an empty tool catalog would alter execution
truth. Catching every projection failure and falling back to defaults would hide
real read failures. The durable history path needs neither behavior: it already
has canonical model-selection controls and an independently authorized summary.

## Consequences

Persisted history opening performs one summary read, creates no Session handle or
projection subscription, and does not depend on the current executable preset.
Draft selection, caller and direct-parent checks, read errors, and execution
restoration rules retain their existing owners. No additional cache or persistent
projection is introduced.
