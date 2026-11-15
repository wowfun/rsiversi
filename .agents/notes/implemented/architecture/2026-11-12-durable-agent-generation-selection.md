---
name: Durable Agent generation selection at settled Steps
---

## Problem

The original [preset generation decision](2026-08-30-agent-preset-generations.md)
keeps resident Sessions pinned and resolves mutable current source on cold resume.
That prevents running-agent plugin replacement and can change execution after
eviction without a durable selection event.

## Decision

Historical executable recovery and full-state publication are superseded by
[current-plugin recovery](2026-11-14-current-plugin-recovery.md). Immutable pins
for admitted work and durable publication fencing remain applicable.

Separate stable Session identity from an immutable executable selection. Fresh
admission and settled-Step replacement atomically commit the recipe address with
the complete active Domain set. Recipes retain the evaluated Profile, exact
artifact identities and linked executable identity. Cold execution reconstructs
that selection; mutable current source is never a fallback.

This supersedes the cold/resident selection clauses in the preset decision and
[Agent Domain state](2026-09-08-agent-domain-state.md), and the fresh-only Domain
creation clause in [typed commits](2026-09-08-typed-domain-commits.md).
It also supersedes permanently pinned resident code in
[Host leaf management](2026-09-20-host-profile-leaf-management.md) and
[native artifact management](2026-09-08-native-artifact-management.md).
Ordinary Domain proposals still cannot change declarations. Requests, Tool batches,
jobs and Program children retain their captured execution owners. A retained draft
or Program pin authorizes cold reuse only when its recipe matches the committed
selection. Both retained and rebuilt generations validate the saved Domain set.
Custom providers explicitly implement restoration; a caller-declared source
digest alone is insufficient executable evidence, so the default resolver refuses
cold recovery. Probe admission is independent of build admission, preventing an
unpublished candidate from serializing unrelated unchanged-source checks.

Capture settled state under Session mutation admission, then release admission
while rebuilding and validating. Publication compares the captured cuts again
under admission. This keeps cancellation responsive without allowing a candidate
to overwrite newer Domain state. The context builder and private extension
state must accept the candidate before publication. Rejection keeps the predecessor;
a later unchanged-source check clears a stale rejection. The
[composition generation contract](../../../../crates/rsi-agent/docs/subsystems/composition-generations.md)
owns the current mechanics.

## Alternatives considered

A mutable registry lets one request observe mismatched definitions and execution.
Restart-only activation cannot serve creator workflows. Persisting only source
paths makes cold execution dependent on later edits. Generic state migration would
invent semantics that only the plugin can define; incompatible candidates are
rejected instead.
The running executable digest remains part of every recipe, including catalogs
without native addons: standard linked factories still contain executable code.
Binding only when native records exist would leave those callbacks unidentified.
Package versions and source labels cannot prove their implementation bytes.
In particular, identical JSON shape does not establish compatibility between
codec versions: reference interpretation and business semantics can differ even
when decoding succeeds. Retained state keeps its exact codec identity. Replacement
diagnostics travel as transient projection status, so operators can see a rejected
update without adding a durable Fact or a second observation service.

## Consequences

Saving evaluated configuration is necessary for exact reconstruction and needs the
same storage protection as Session state. Historical recipes remain reachable while
their canonical controls remain retained. Live replacement does not move existing
background work to new code. Generation tests cover settled boundaries, rejection,
eviction, source deletion, retained-pin identity and exact CAS reconstruction.
Cold recovery reconstructs retained native records through the existing Loader
and requires the exact linked executable digest. A changed binary cannot restore
old callbacks. Preserve that binary with its state, or explicitly reset to empty
history; there is no automatic migration. Executable hashing is cached once per
process, and its initial cost scales with binary size. Native library
replacement under the same executable remains recoverable while artifacts exist.

