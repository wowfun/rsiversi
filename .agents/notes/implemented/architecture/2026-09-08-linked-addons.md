---
name: Standard linked addon declarations and configuration descriptions
---

## Problem

The standard product repeats Agent factory identities between a contribution
catalog and its preset compiler. External products cannot contribute all faces
of a domain through one explicit composition declaration.

## Decision

The product uses immutable addon declarations for linked factories, scoped Profile fragments,
exact markers, target platforms and descriptive configuration metadata. Derive
Host registrations, Agent allowlists and launch identity from those declarations.
Built-in capabilities use the same builder and registration path. Separate the
Service, Agent, Application and Client roles; endpoints remain ordinary plugins
whose Profile controls remote exposure. Build-time document assets belong to
application factories. Preserve one Runtime and explicit parent authority.

Factory schemas describe authoring without executing factories or duplicating
prepare validation. Exact repeated marker declarations are shared during product
assembly; conflicting types or keys fail before Host build. The generic Host
continues to freeze its own catalog and reject duplicate registrations.

## Alternatives considered

A linker inventory introduces ambient discovery. An extra schema validator
creates a competing normalization/requirements authority. Repeating factory IDs
for preset validation lets preview accept a different product from execution.
The generic Host accepts resolver-owned Native identities through the
[resolved catalog decision](2026-09-09-host-resolved-catalog.md). Standard product
native artifact management remains separate from linked addon declarations.

## Consequences

An independent linked addon loads, configures, executes and withdraws through
composition alone. Public behavior tests cover duplicate rejection, prepare-free descriptions,
Agent compiler/catalog agreement, identity changes, target checks, and matching
embedded/remote domain faces. Owning tests and documentation validation pass. The standard built binary also
passes isolated live coding and Chromium/Firefox application scenarios; these
checks do not establish behavior for arbitrary third-party addon code.

A declared client does not grant remote authority; server registration and
connection access still govern requests. Linked implementations and document
assets are trusted application code. Descriptions may contain no credentials;
they are bounded product metadata intended for inspection.
