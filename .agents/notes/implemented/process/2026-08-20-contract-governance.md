---
name: Contract ownership and executable interface documentation
comment: Minimal governance for semantic changes across the product family
---

## Problem

The repository already has product-owned documentation, schemas, typed
protocols, and conformance suites, but a Rust item can still become public
without explaining the invariants, ordering, failures, or effects that its
caller must understand. Repeating those facts in a separate catalog would
create another source that can drift, while changed-file or source-text gates
would mistake textual correlation for behavioral evidence.

## Decision

Every stable fact has one authority at its owning seam. Schemas and source
constants own exact machine shapes and expressible bounds, rustdoc owns the
contract of one public Rust interface, product subject documents own
cross-package temporal and durability semantics, and Agent Notes retain only
decision rationale. A semantic change updates that authority before its
implementation and ships with the closest behavior or invariant test.

CI denies warnings while building every root-workspace library's rustdoc, then
executes doctests. Rustdoc records only facts that callers cannot derive from an
item's name, type, or enclosing interface. An exported item that is not part of
an intended interface is made private instead of being documented into a new
accidental contract. Existing schema, ABI, wire, and product conformance suites
remain the evidence for their seams.

## Alternatives considered

A central contract registry and stable contract identifiers were rejected
because they duplicate product ownership and require a second inventory to
stay synchronized. Public-interface snapshots and changed-file coupling were
rejected because this pre-release repository permits intentional interface
correction and those checks report structural churn rather than semantic
failure. Grandfathering undocumented items behind a baseline was rejected
because it would make the baseline durable debt.

Compiler-enforced `missing_docs` was rejected because it checks prose presence,
not caller meaning. Workspace-wide enforcement also covers tests, binaries, and
build scripts, where it rewards tautological comments; target-specific linting
would replace those comments with suppression attributes without proving a
contract.

## Consequences

The rustdoc gate catches broken links and warning-level documentation defects,
but reviewers decide whether an interface needs prose and whether that prose
captures the caller contract. The repository carries no documentation-coverage
inventory or baseline. Behavior remains free to evolve before release, but an
intentional change must update the one authority and its evidence together; no
compatibility shim is implied.
