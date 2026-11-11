---
name: Installed payloads retain one copy of their file bodies
---

## Problem

PackageStore retained an uncompressed payload container beside its materialized
tree. A measured Linux DSH installation contained 811,801,290 container bytes and
802,619,123 expanded body bytes; the indexes account for only 9,182,167 bytes.
Ordinary acquisition read both copies. Identical-content sharing between
expanded files could save at most another 16,944,732 logical bytes in that
fixture, so it does not address the dominant duplication.

## Decision

The [PackageStore contract](../../../../crates/rsi/packages/README.md) makes the
transport container temporary input. A payload export authenticates its exact
index, which in turn authenticates every materialized file. Admission verifies
the source container while streaming its files directly into the unpublished
object, retaining only the index and the expanded tree. Ordinary acquisition
checks the index and runtime files; process acquisition retains its existing
metadata and lifetime policy.

DSH's `packages/boot/plugin-manager/src/operations.ts` delegates installation to
pnpm and resolves bundle directories through app-boot. Application loading does
not require a second DSH-owned uncompressed image of the installed tree. RSI
adopts that distinction between distribution input and runtime files, retaining
its package integrity, publication and lease contracts.

## Alternatives considered

Compressing the retained container reduces bytes but leaves two durable body
representations and another verification read. Cross-package hard-linking or a
new file-level CAS adds ownership and collection machinery while addressing a
much smaller measured duplicate set. Reconstructing the whole archive digest on
every acquisition removes stored duplication but forces ordered content reads;
an explicit authenticated index preserves the existing bounded file verifier.

## Consequences

Tests establish single-copy installed storage, rejection of malformed source
containers and altered indexes/files, preserved atomic publication, leases and
GC, and single-count capacity accounting. Current DSH package producers include
the index commitment. Windows and Linux targeted tests and the real Linux
market/workbench workload exercise the changed representation. The measured
nine-package installation removes about 803 MB of duplicate body storage.
This does not imply a corresponding runtime memory reduction.

Old payload manifests without the index commitment are inadmissible. Existing
immutable objects are never rewritten; current packages must be produced and
installed into a fresh fixture for acceptance. The complete source digest and
the runtime index commitment have distinct purposes and must not be confused.
Optional package-manager caches and the installed service executable remain
outside this removal of PackageStore body duplication.
