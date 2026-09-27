---
name: Manifest coverage and limits of the application ownership gate
---

## Problem

Cargo overrides can redirect reusable-library dependencies into applications
without a direct application path in the library's dependency table. Explicit
Cargo target and build-script paths can cross the same boundary without adding
a dependency. A passing manifest check must not imply that arbitrary source
includes or invoked commands were analyzed.

## Decision

The architecture gate checks reusable packages and their owning workspaces,
including standalone workspaces. It rejects application paths in normal, dev
and build dependencies, inherited declarations, workspace patch/replace tables,
and explicit Cargo target or build-script paths. Workspace patch/replace overrides are checked
even when currently unused: their future or transitive use must not silently
reverse source ownership. The tool README defines the mechanical coverage;
source includes and process coupling still require ownership review.
Independent read and path errors are collected before returning sorted diagnostics;
one broken declaration cannot hide a validly resolved reverse dependency. Source
directory symlinks inside the repository are traversed once per physical directory,
with cycles deduplicated and escapes rejected.
The application root is canonicalized as well, so an in-repository `apps/`
symlink cannot conceal a reverse edge. Path comparisons use whole components.

## Alternatives considered

Resolving a complete Cargo graph would make this read-only governance gate depend
on dependency availability and resolution configuration, while still missing
arbitrary build-script behavior. Matching application-looking strings in source
would confuse comments and diagnostics with dependencies and still miss computed
paths. Neither establishes the stronger guarantee.

## Consequences

Failures remain deterministic and include the manifest and declaration. A
workspace cannot reserve an unused override into an application for later use.
Passing this gate is evidence about Cargo declarations, not a proof of all source
or runtime coupling; the repository ownership rule remains broader than the gate.
