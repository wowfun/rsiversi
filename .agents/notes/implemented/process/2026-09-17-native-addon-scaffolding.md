---
name: Independent native Tool addon scaffolding
comment: Offline generation from a maintained safe SDK template
---

## Problem

The native addon fixture proves several unrelated extension contracts together.
Copying it for a first Tool addon exposes probe-only ports and hides the explicit
Portable bridge needed by an Agent preset.

## Decision

The repository tool generates a separate workspace from a stripped, maintained
echo template. Generation performs no build or dependency resolution, preserves
the template's lock graph, and publishes a complete private sibling directory
without replacing an existing destination. Names use nonempty lowercase ASCII
segments separated by single hyphens; destination paths reject control characters.
Lock rewriting selects exactly one source-less template package structurally and
preserves the dependency graph. Public SDK paths identify this exact
checkout; the generated documentation states their relocation cost. Installation,
enablement and preset edits remain explicit existing product operations.

## Alternatives considered

Copying the entire fixture would retain unrelated probes. Resolving dependencies
at generation time would make file creation depend on Cargo configuration and
network state. Embedding a second SDK copy would obscure which ABI is exercised.

The [relocatable SDK decision](2026-09-24-relocatable-native-addon-sdk.md)
replaces only the absolute-path distribution choice.

## Consequences

An external project can build offline against cached dependencies and inspect a
small Describe/Execute implementation. The SDK checkout must remain available.
Atomic no-replace directory publication currently restricts the command to Linux
and WSL; other hosts receive an explicit unsupported error.
