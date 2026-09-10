---
name: Workspace cursor order survives deletion and restart
---

## Problem

An insertion-order cursor outlives the registration at its boundary. Reconstructing
allocation from surviving records reuses deleted orders after restart, so an old
cursor can silently skip a later registration. The empty registry loses every
allocation witness.

## Decision

The [Workspace contract](../../../../crates/rsi-workspace/README.md) uses a
separate durable allocation high-water mark. Allocation precedes registration;
the registry never removes that mark during deletion. A failed creation can leave
a gap, which is safe for exclusive continuation. Recovery validates registrations
against their allocation witness.

## Alternatives considered

Taking the maximum surviving order cannot recover deleted history. Assigning a
random generation to every cursor would invalidate continuation at every restart
and change the protocol rather than preserve its promise. Retaining deleted
registrations as allocation witnesses would grow storage without bound.

Automatically treating old domain data as complete would conceal irrecoverable
allocation history. Pre-release format rejection is explicit and leaves the
operator's files intact; silently resetting the registry is unacceptable.

## Consequences

Workspace domain version 3 rejects earlier formats. Existing path-derived IDs,
Session headers and Store formats do not change. Creating a registration requires
two ordered domain writes instead of one. The extra bounded metadata record and
possible allocation gaps avoid a cross-record transaction or a new Storage API.
Public registry tests cover deletion of the highest records, an empty registry,
restart, failed registration after reservation, and malformed allocation data.
