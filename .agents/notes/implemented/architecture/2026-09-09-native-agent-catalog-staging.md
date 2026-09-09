---
name: Explicit native Agent catalog staging with one Loader
---

## Problem

Installed native bytes and enabled metadata do not establish that an Agent can
use them. A manager that independently updates its compiler and executable
catalog can admit mixed identities. Recreating a Loader after failed native
finalization would bypass the existing retained-resource admission fence.

## Decision

The product's [native addon manager](../../../../crates/rsi/core/README.md)
consumes the [non-executing store](2026-09-09-native-addon-source-storage.md)
and one explicitly supplied NativeCatalog. Its constructor is inert. Explicit
blocking refresh checks selection identities, target, collisions and factory
capacity before loading; each load verifies the expected artifact digest before
code execution and checks the returned ABI plugin identity. Publication checks
that the selected records still match after staging.

One published value pairs the preset compiler with its complete executable
contribution catalog through the existing
[Agent snapshot seam](2026-09-09-agent-composition-snapshots.md). Rebinding a preset
compiler preserves its roots, default store and shared authoring lock. Pending
or failed staging rejects a new snapshot, including a cache hit. Existing Session
pins keep their own catalog and native leases. The manager only admits Agent
factories and explicit generation-private Portable keys; fixed linked declarations
remain the base of each snapshot.

Retained finalization closes manager admission permanently. The manager exposes
categorical, path-free selection and actual Loader resource observations, keeps
the same Loader, and never deletes its cache. Closing new selection does not
revoke old pins or imply native cleanup. This explicit library seam does not
start background work; its caller must supervise blocking staging to completion.
The [remaining management proposal](../../proposed/architecture/2026-09-08-native-artifact-management.md)
still covers automatic Runtime integration and build/watch actions.

## Alternatives considered

Loading from a snapshot query would run native code while the Agent builder is
selecting its frozen input. Publishing a compiler separately from the factories
would weaken the single-snapshot contract. Reinstall-as-enable would silently
change running user intent. Rotating catalogs or cache directories on native
failure would evade the retained lease and capacity policy. Source-only cache
keys would reuse old code after a valid artifact replacement.

## Consequences

Two compiled native fixture variants demonstrate a changed Tool definition under
the same Profile, while the old pin preserves its own definition and provenance.
A paused real IDENTITY callback demonstrates concurrent close/selection changes
rejecting an unpublished candidate and preserving cleanup ownership. A separate
child-process fixture deliberately fails FINALIZE: staging and its catalog lease
remain retained, manager admission closes, and the cache stays locked after
ordinary owners drop. Process exit bounds that intentional failed-cleanup test.
These checks distinguish successful release from expected failure retention;
they do not simulate a library-close failure or establish automatic update wiring.
