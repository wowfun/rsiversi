---
name: Generated wire contract admission
---

## Problem

Executable build families couple independently shipped clients, while manually
versioned operation names alone do not detect changes to their wire models.

## Decision

Owning APIs publish generated descriptors covering normalized Serde models,
custom codecs and their source dependency closure. Each protocol has an explicit
semantic model version. Connection admission compares required descriptors;
unrelated optional APIs do not participate in a client's compatibility decision.

Committed schema snapshots make the exact changed contract reviewable. The
repository gate is read-only unless explicitly asked to write snapshots and
requires a version increase for changes relative to its Git baseline.
Contract names have one owner across the workspace; duplicate owners are rejected
before their descriptors can be treated as interchangeable authorities.
Native ABI layouts remain under their existing independent rules.

Explicit source inputs cover non-Rust codecs and framing. The Web document/Worker,
Desktop JavaScript bridge and frontend control exchange have separate owning
contracts. Source text is bounded, confined to the workspace and normalized for
line endings. Frontend control checks its exact descriptor in every frame.
The owning [schema generator](../../../../crates/rsi-api/schema-build/README.md)
and [protocol gate](../../../../crates/tools/rsi-xtask/README.md) define their inputs
and baseline rules.

## Alternatives considered

Hashing a whole build includes unrelated code and toolchains. JSON Schema alone
does not capture custom Serde implementations or semantic model changes.
Normalized Rust model and codec snapshots deliberately err toward rejecting a
potentially changed contract; owner tests establish its actual semantics.

## Consequences

Same-version changes, missing interfaces and malformed fingerprints are rejected.
Independent clients can connect when the interfaces they actually use match.
Generator tests cover dependency features, source changes, line-ending stability
and workspace escape refusal. A shared rejection corpus and real Vite process
tests exercise frontend framing and readiness alongside the Rust decoder.

Normalized source snapshots are conservative, not a semantic equivalence proof.
Binary codecs require their header types and numeric discriminants as well as
their function bodies. External dependency versions and enabled features can
change admitted values without changing local DTO declarations. Owning behavior
tests and explicit version review remain necessary; the gate must not imply
coverage for a dependency or validator it does not actually track.

The verifier memoizes parsed package fragments for one invocation and validates
all owners before write mode changes any snapshot. Cargo builds emit descriptors
without redundant output-directory snapshots. External dependency identities
remain conservative admission inputs: a version-only update is not itself
evidence that externally implemented serializers accept identical wire values.

Baseline owners are matched by contract name rather than today's path, so an
owner move preserves version review and a deletion cannot silently bypass it.
Unknown snapshot formats reject; format migration does not implicitly certify
wire equivalence.

The retained source closure is deliberately more conservative than a structural
wire model. Runtime and target-specific dependency edits can still invalidate
unrelated admissions. That cost remains until owners define and test complete
model dependency boundaries; this gate does not claim semantic compatibility.
Format 2 removes test-only inputs, normalizes explicit Rust codec sources, and
commits the exact hashed bytes. All current contract versions increase together
for this pre-release generator migration; mixed generations must rebuild.
Build scripts refuse stale reviewed snapshots, so native and JavaScript peers
cannot silently advertise different descriptors from one checkout.

Exact-byte snapshots use pretty-printed JSON so reviewers can see changed model
entries. The representation migration raises the current pre-release protocol
versions together; the digest still comes from the committed bytes. Descriptor
identity has a shared Rust/generator/JavaScript conformance corpus. Cargo tracks
scanned source directories so newly added files cannot skip the admission gate.
