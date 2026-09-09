---
name: Product-owned native artifact management
---

## Problem

The [foundation-first decision](../../implemented/architecture/2026-08-27-foundation-first-plugin-composition.md)
rejects installation and artifact watchers. The approved roadmap additionally
requires local artifact management and limited Agent contribution hot updates.

## Proposal

Partially supersede that product restriction after native Tool/Model bridges
work. Meta/native-loader retain explicit trusted-path loading without package
discovery, installation, version solving, or watchers. Standard RSI owns local
manifests, build/staging, and immutable Agent catalog snapshots; install and
enable remain separate.

Host preserves explicit embedder-supplied linked/native provenance in its frozen
digest. Generations capture source/catalog/artifact identity together. Existing
Session pins retain old code. Global provider routes and linked/Worker code use
restart. Successful finalization permits cleanup; failed finalization/close
retains mapping, artifact, lease, and accounting and closes catalog load admission
until process recovery. Management cannot bypass this with another catalog.

## Alternatives considered

The [local source store](../../implemented/architecture/2026-09-09-native-addon-source-storage.md)
and [explicit catalog staging](../../implemented/architecture/2026-09-09-native-agent-catalog-staging.md)
now supply installation and immutable Agent selection. Automatic Runtime wiring
and build/watch actions remain proposed. A Meta installer would violate its
generic ownership. Remote marketplaces and version
solving are unnecessary for local artifacts. Forced unload risks use-after-free.

## Acceptance criteria

Real native Tool/Model fixtures precede management. Tests cover frozen identity,
generation pins, candidate failure, successful cleanup, and retained-failure
admission closure. The foundation note keeps its remaining authority and links
the partial supersession.

## Risks

Native code remains trusted in-process code. Failed unload can retain resources
until process recovery; universal return-to-baseline claims are invalid.
Artifact identity is provenance, not an authorization credential.
