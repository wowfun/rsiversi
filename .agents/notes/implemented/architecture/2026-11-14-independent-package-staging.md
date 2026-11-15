---
name: Independent package staging
---

## Problem

Holding capture exclusion across download and build made unrelated new Agent
steps wait for network and compiler work. Publishing importer output directly
into persistent profile roots also confused candidate lifetime with committed
selection. Queued management calls could close captures before becoming writers.

## Decision

A single Cordis runtime uses one reentrant profile writer lane. Writers prepare
immutable candidates before requesting the existing request boundary. Only commit
and compensation exclude captures; old views drain before any executable switch.
The Store admits objects and pins candidate closures within one writer critical
section. Process leases, rather than persistent staging profiles, own candidates.
The package contract belongs to [Packages](../../../../crates/rsi/packages/README.md).

Application protocol 13 exposes disposable stage handles. The existing roots CAS
retains old and new closures during publication; the existing journal remains the
sole recovery authority. [Serve](../../../../apps/serve/README.md) owns this wire
contract. Candidate resolver maps are detached until commit. Managed replacement
prepares first instead of removing the old bundle; raw package operations keep
their separate activation ownership and partial portable-restore semantics.

Cancellation of staging is cooperative across both application transport hops
and the native handler. A confirmed cancellation means importer shutdown and
admission have settled. Loss of that confirmation is an error, never an inferred
successful cancellation. Commit becomes non-cancellable before recovery capture.
The JS forwarding hops retain a staging slot until cleanup replies or the
connection closes, rather than applying the ordinary five-second cancellation
acknowledgement deadline to filesystem cleanup. Slot capacity still bounds
unsettled work. A stalled drain requires peer retirement; choosing a larger
arbitrary timeout would still reject valid slow cleanup.
[Extension Core](../../../../crates/rsi-extension/core/README.md) owns handler
retirement; ordinary handlers retain their existing cancellation behavior.

DSH's management serialization, registry selection, build approval and result
semantics remain authoritative. Approval is durable operator policy, independent
of installation success. Windows environment lookup follows DSH's case-insensitive
semantics even inside Node workers.

## Alternatives considered

A second Cordis runtime requires service and resource handoff unrelated to download
latency. Persistent staging profiles require abandoned-job recovery and duplicate
state. Moving only the import call leaves queued writers able to close captures
early. Parallel candidates require a conflict protocol without benefiting this
serialized management contract. None is necessary for keeping Agent steps running.

A callback cannot await management that needs its own view to drain. Admission
rejects this cycle before the profile writer queue; the management Tool explicitly
detaches its accepted operation from callback context. A watchdog would diagnose
the same ownership error later while blocking unrelated captures in the meantime.

## Consequences

Preparation does not publish a manifest, resolver binding or persistent candidate
root. Cancellation and process loss retire temporary leases; existing GC owns
unreferenced bytes. Recovery restores exact digests, profile bytes and links after
an interrupted commit rather than re-resolving mutable sources. A child package
command shares its validated parent's boundary even across an IPC callback.

Commit latency still includes old callbacks and plugin cleanup/initialization.
Approved build scripts retain native authority, and CPU or disk contention remains
possible. These limits do not justify blocking captures during preparation.
