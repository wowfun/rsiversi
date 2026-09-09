---
name: Immutable Agent compiler and executable snapshots
---

## Problem

A Profile source digest cannot identify a generation after its native artifact
changes. Independently reading a mutable compiler allowlist and factory resolver
can also combine authorities that were never published together. Retiring old
code at catalog replacement breaks the existing Session pin contract.

## Decision

The standing builder consumes one application-owned immutable snapshot containing
the preset catalog/compiler and exact contribution catalog. It captures that
snapshot once after build admission and before compilation. Source failure is a
selection failure even when a previous generation is cached. Resolution and
activation retain the captured snapshot across concurrent replacement.

The generation cache compares the compiled program with complete factory
identities, update modes, nominal Local/event bindings and declared Portable
isolation keys. Its effective source digest includes those inputs. Nominal Rust
identity hashing is process-local; exact typed equality remains authoritative for
cache reuse. A linked factory's declared revision must change with its code.
Each generation allocates its declared fresh Portable mappings before any leaf
activates, keeping overlapping providers and bridges in their own generation.

The existing Scope and opaque pin remain the sole lifecycle owners. The Scope
retains the entire selected catalog through cleanup, including unselected
factories. Supersession does not revoke old pins. No artifact discovery, installer
or mutable catalog API is granted to Agent consumers.

## Alternatives considered

A source-only cache reuses obsolete code. Reading compiler and factories through
separate update channels permits mixed builds. Replacing every Session's pin
changes admitted Tool/context behavior and violates durable execution ownership.
A second artifact lifetime registry duplicates the existing Scope and native
loader leases. Persisting Rust TypeId encodings would invent a cross-build ABI.

## Consequences

Public tests cover unchanged-source catalog replacement, replacement during
blocked activation, source failures, typed marker/isolation/mode cache changes,
real Portable provider bindings and final-pin catalog reclamation. Catalog entry
and key bounds apply before generation work; Runtime Context budgets can be
stricter. The product's [native catalog staging owner](2026-09-09-native-agent-catalog-staging.md)
consumes this seam; Agent composition itself does not implement installation,
automatic updates or native failure recovery. Preset compiler rebinding preserves
root/default/authoring authority while existing clones keep their own compiler.
