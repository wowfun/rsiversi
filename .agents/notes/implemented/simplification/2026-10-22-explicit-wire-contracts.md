---
name: Explicit wire contracts and conformance
---

## Problem

Source-closure fingerprints reject compatible implementation and dependency
updates. They catch unreviewed changes conservatively but do not prove incompatibility.
This supersedes the source-closure policy recorded in the archived
`2026-10-20-wire-contract-gates.md`; its original rationale is preserved verbatim.

## Decision

Protocol identity is the owning name and explicit semantic version. Calls also
check read/mutation/stream shape because uncertain mutation outcomes depend on it.
Owner wire corpora and real peer tests replace source fingerprints. The contract
verifier checks declarations and unique ownership across crates, applications and
fixtures, including nested owners, without a Git baseline. It reports independent
declaration errors together in stable path order and omits generated build trees.

## Alternatives considered

Keeping source snapshots only for tests preserves their implementation coupling.
Removing all call-shape checks can misclassify completed mutations. Full metadata
equality also couples independent authorization and resource policies.

## Consequences

Authors must version incompatible semantics explicitly; tests establish selected
wire cases, not universal equivalence. Source fingerprints no longer decide
protocol admission.
Native ABI, artifact integrity and execution leases retain their independent owners.

A source-diff version-bump tripwire is intentionally not used: implementation-only
changes do not establish wire incompatibility, while a broad justification escape
would not prove peer compatibility. Owners review semantic changes against their
encoded-value and peer tests rather than restoring a source-change gate.

The former `rsi.web.worker` wire identity is retired. The worker's JavaScript/Wasm
entry points ship in one paired bundle: `apps/web/worker.js` checks the document's
bundle identity and the Rust `build_family()` export before dispatch. Its consumed
Host APIs retain their owning declarations, selected by the worker package's
`host-interface-packages` metadata. The separate native Desktop boundary retains
`rsi.desktop.bridge`. An independent descriptor for the internal bundle entry
points would imply peer compatibility they do not support. A separate global
protocol inventory would duplicate owner declarations and restore a baseline gate
without proving compatible behavior.
