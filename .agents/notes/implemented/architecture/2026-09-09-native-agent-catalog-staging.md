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

Native source compilation derives its allowlist and environment identity from the
frozen base declarations and exact enabled records. Runtime staging and explicit
preset authoring share that pure derivation. ABI update modes and resolved factory
identities remain independently hashed by the executable generation catalog.
This lets authoring validate declared source without executing native code or
inventing placeholder factories. One authoring request captures one immutable
selection; later requests see enable/disable changes. Healthy source remains
separate from artifact availability, ABI checks, prepare and activation. Base
catalogs and Host launch identity remain frozen, and pure Host preview does not
read or initialize native storage. Default and deletion commands do not depend
on native source health, preserving their existing repair semantics.

Retained finalization closes manager admission permanently. The manager exposes
categorical, path-free selection and actual Loader resource observations, keeps
the same Loader, and never deletes its cache. Closing new selection does not
revoke old pins or imply native cleanup. This explicit library seam does not
start background work; its caller must supervise blocking staging to completion.

The standard Unix Host supplies that supervision through an ordinary linked
manager plugin. Activation owns one Loader at its fixed cache path, requires the
Service Owner Local, and publishes the Agent source and management Locals. Its
single worker stages an initial selection and polls changed selections. Explicit
refresh has a bounded queue; cancelled queued requests are skipped, while an
in-flight native callback remains joined. Retirement closes admission and drains
the worker before releasing the Service Owner dependency. The frozen factory
retains only construction inputs, so pure preview never opens the source store
or Loader, and a clean shutdown can release its cache lease. Unchanged failed
candidates require explicit retry rather than repeated native execution.
The [product management decision](2026-09-08-native-artifact-management.md)
keeps build/watch source production separate from this runtime staging owner.

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
they do not simulate a library-close failure. Standard Host probes additionally
verify automatic selection updates, cancelled bounded refresh waiters, and actual
Service Owner lock exclusion until the blocked native callback has drained.

A separate ordinary API plugin owns the local mutation for explicit refresh,
with typed path-free failures and finite receipts. Its terminal application uses
the existing-owner operator connection. Keeping API registration out of the
source manager avoids delaying source initialization behind API availability.
Read-only Inspector operations are not expanded into administrative authority.
API retirement cancels its Control waits before draining registration, closing
queued reply receivers. The original worker skips abandoned queued requests and
still joins any in-flight native work when the manager retires. Hard dependencies
retain the Control owner through API cleanup. This is an adapter over the same
Control and Loader, not a second lifecycle or a way around failure retention.
