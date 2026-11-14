---
name: Partial Profile convergence with owner-input compensation
---

## Problem

Whole-tree compensation after an optional leaf fails can remove healthy services
and obstruct the control surface needed to repair configuration. The earlier
[binding convergence decision](2026-09-08-profile-binding-convergence.md) did not
distinguish ordinary configuration reload from executable input replacement.

## Decision

Ordinary startup and source reload retain successful leaves, continue in source
order, and report partial degraded observation. Product bootstrap selects which
security, control and persistence owners must be Active before work begins.
Pre-staging preparation follows that division instead of preparing every optional
leaf and restoring whole-Profile failure indirectly.

This supersedes the earlier ordinary-reload compensation rule. Explicit
[owner input replacement](2026-09-10-profile-input-replacement.md) retains its old
resolver and compensates executable changes. Compensation restores the usable
membership present before the attempt, including its exact bindings, rather than
starting absent or failed leaves from the prior desired tree. It preserves the
prior desired target and degraded diagnostic, allowing a later ordinary reload to
retry missing leaves. Failed compensation publishes the observed degraded graph.
The [Profile contract](../../../../crates/rsi-meta/profile/README.md) owns behavior
and limits.

## Alternatives considered

Whole-tree rollback makes independent configuration failures affect healthy
siblings. Always retaining partial success during executable input replacement
would discard its explicit prior-input recovery promise. Blocking replacement
whenever health is degraded would prevent the operation most likely to repair it.

## Consequences

Partial success is observable and is not atomic replacement. Products must identify
their required owners explicitly. Replayable plugins prepare only when activated;
static generation construction retains complete preparation proofs separately.
Tests cover successful siblings, retry, exact binding compensation, replacement
after partial reload, watcher behavior and capacity at the existing graph size.

