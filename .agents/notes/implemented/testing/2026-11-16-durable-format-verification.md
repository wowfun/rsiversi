---
name: Durable format verification
---

## Problem

The current decoder can round-trip its own writer while silently invalidating
previously persisted digest inputs. A fresh database test does not preserve
evidence from the writer that preceded a format edit.

## Decision

Each persistence owner declares its executable conformance target in
`persistence-contract.json` and owns immutable corpora under its package.
`cargo xtask verify-persistence` discovers these declarations, checks retained
fixture bytes against an optional exact Git base, then runs each owner's target.
CI runs declaration and retention checks with `--contracts-only`, independently
of fetching the base; existing package jobs run the executable conformance tests.
`RSI_PERSISTENCE_BASE` supplies the exact base when available. A failed fetch still
fails CI. The initial introduction has no older corpus to retain.
A same-version change can add coverage but cannot replace retained corpus files.
Version bumps keep old corpora
as must-read or must-refuse cases. Runtime schema and byte checks stay in their
owning packages, beside the writer and decoder. Package identity comes from the
sibling Cargo manifest. Payload fixtures prove serde codec stability; integration
owners exercise actual storage writers and envelopes. The payload gate enumerates
retained versions and requires owner read/refusal coverage for older samples.
Byte retention covers non-Markdown corpus files. Markdown contains editable
procedures and provenance, not durable writer bytes; the shared testkit owns the
payload capture procedure and owners retain only their specific provenance.

## Alternatives considered

A repository-wide source hash overapproximates format ownership and rejects
ordinary refactors. A previous-to-current schema chain implies an upgrade
guarantee that this pre-release product does not make. Rebuilding golden files
from the current writer on every check erases the evidence being checked.
Unlike the rejected [wire tripwire](../simplification/2026-10-22-explicit-wire-contracts.md),
these byte fixtures preserve durable digest inputs, custom serde and opaque values
whose identity outlives the writer. Object field order remains significant.

## Consequences

The gate rejects changed or removed retained fixtures and removed owner
declarations, permits additive fixtures, and propagates owner test failures.
The Store corpus verifies normalized SQLite structure and exact Header, Fact
and control encodings using output from its actual writer. Unsupported versions
are rejected on a disposable copy without modifying retained bytes.

A finite corpus does not cover all legal values or serde variants. Owners must
add schema and negative decode coverage for newly introduced format surfaces.
The Git base check requires CI to fetch the exact comparison commit.
