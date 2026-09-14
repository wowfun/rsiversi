---
name: Definition-owned domain initialization on fork
---

## Problem

Unconditional DomainBaseline construction overlays every parent domain on child defaults. This
would copy parent Todo state and override a child's request-derived model
baseline even when those domains require independent initial state.

## Decision

Each immutable domain definition declares Inherit or ResetToInitial. Inherit
remains the default. Baseline construction validates inherited snapshots and
applies the definition's policy before the child is admitted. Model selection
and Todo use ResetToInitial; Kernel never recognizes their domain names.

## Alternatives considered

UI-only hiding leaves incorrect durable state. Resetting after the first model
request exposes parent state during creation and recovery. Kernel name checks
couple optional plugins to the generic state owner.

## Consequences

Domain baseline tests verify reset versus inherited state and reject malformed
inherited snapshots. Existing bounded history-fork and cold recovery tests retain
the same lineage contract.


The policy is generation-owned, so cold composition must retain the ordinary
strict codec and generation checks rather than silently invent missing state.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/composition-protocol/README.md).
