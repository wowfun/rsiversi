---
name: Frozen Host catalog preserves resolver-owned factory provenance
---

## Problem

Host reconstructing every registration as Linked loses NativeCatalog artifact
identity and makes composition cache keys blind to native code replacement.
Generic Host must accept explicit native composition without becoming a loader
or a product installation manager.

## Decision

HostBuilder accepts ResolvedFactory and freezes its complete identity and static
update mode. The trusted embedder supplies NativeCatalog results; Host validates
shape and bounds without re-attesting artifact provenance. Linked registration
uses the same path. Composition digests distinguish identity variants and hash
native artifact bytes by their resolver-provided SHA-256. Meta's consuming
into_parts allows trusted composition to wrap implementation destruction while
preserving identity and policy. Factory destruction is contained before Host
validation, including rejected registrations.

The authoritative contract is [rsi-host](../../../../crates/rsi-host/README.md).
The [linked addon decision](2026-09-08-linked-addons.md) continues to own standard
product declarations; this generic Host seam does not implement installation.

## Alternatives considered

Rewriting Native identity into a linked revision misrepresents provenance and
lets linked/native representations collide in composition digests. Teaching Host
to discover or load artifacts couples a generic frozen resolver to product path,
installation and failure-retention policy. A mutable post-build registry breaks
Profile and Session generation identity. These alternatives are rejected.

## Consequences

Public tests cover native identity in preview and Runtime, identity-sensitive
digests, shared admission, malformed digests and rejected/accepted destructor
panics. A real ABI v3 dylib loaded by NativeCatalog executes through Host; its
private staging remains pinned through Runtime shutdown while the frozen catalog
is owned, then releases after the last Host owner. Durable cache entries remain
for explicit management. Native finalization failure semantics remain with the
Loader; Host does not bypass retention by creating another catalog.
