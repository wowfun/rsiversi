---
name: Product-owned native artifact management
---

## Problem

The [foundation-first decision](2026-08-27-foundation-first-plugin-composition.md)
excludes package discovery from generic Host and Loader ownership. Local native
business contributions additionally need explicit source management without
weakening native failure retention or existing Session generation pins.

## Decision

The standard [RSI product](../../../../crates/rsi/core/README.md) owns local
manifests, content-addressed source objects, explicit build/watch and immutable
Agent catalog selection. Installation and enablement are separate source
mutations; their receipts are independent of Runtime convergence. This partially
supersedes the foundation's product installation restriction while preserving
the generic Meta/native-loader trusted-path contract.

One ordinary manager owns a fixed Loader and publishes the compiler, selected
factories and source identity together. Each Agent generation freezes the exact
catalog/artifact identity. Existing resident work retains its old code. Global
provider routes and linked/Worker code require drain/restart. Explicit builds use
ordinary Process and Sandbox plugins; watching changes no Loader admission rule.

## Alternatives considered

A Meta installer would violate generic ownership. Remote marketplaces and version
solving add no necessary authority for explicit local artifacts. Enabling by addon
id after building could select another producer's bytes or undo an operator's
disable, so conditional enable compares both exact installation and prior
selection. Forced unload risks use-after-free; creating another catalog would
bypass failed-retention accounting and capacity.

## Consequences

Native code remains trusted in-process code. Successful finalization permits
normal resource release. Failed finalization or library close retains mapping,
artifact, lease and accounting and closes new load admission until process
recovery. Management cannot prune those retained artifacts or reopen admission
through a replacement catalog. Source objects also remain bounded retained
entries; source garbage collection is not implemented.

Real Tool/Model dylibs, generation replacement, blocked callbacks and explicit
failed-finalizer fixtures exercise these public seams. The linked independent
addon separately covers draft, durable state, commands, Settings and actual UI.
Neither identity provenance nor a content hash is an authorization credential.
