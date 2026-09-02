---
name: Resident Turn claims and recoverable publish ownership
comment: Share immutable identity and return unpublished bodies at capacity boundaries
---

## Problem

Each Turn claim duplicated the complete session Header and exposed mutable
public fields even though Kernel alone issues and validates claims. Every model
delta publication then compared the Header contents and recomputed its
fingerprint. Executor also cloned every Fact body before publish solely to
recover from an uncommon pending-capacity branch represented as a flush error.

## Decision

One resident session owns an `Arc<SessionHeader>`. A `TurnClaim` shares that
allocation, keeps all fields private, and exposes borrowed getters. Kernel
validates its private issuer seal, current live-claim identity, and Header
pointer identity before live operations; cold public boundaries still return
owned values.

Publish consumes owned bodies and returns `PublishAttempt::Published` only
after committing shared Facts. If the session's projected pending bytes require
a durability pass, it recovers the exact bodies with `SessionFact::into_body`
and returns `FlushRequired`. Contention for the process-wide atomic byte
reservation waits within the publication deadline and returns
`TurnError::Capacity` if pressure remains. `TurnError::Flush` represents only an actual
durable flush or shutdown failure. Ordinary Store access failures use the
separate `TurnError::Store` class.

## Alternatives considered

Comparing a cached Header fingerprint would avoid hashing but would preserve a
parallel resident identity representation. Keeping public claim fields with a
private signature would still invite caller mutation and repeated validation.
Cloning bodies only after capacity failure is impossible after ownership has
already entered the Kernel; returning the consumed values makes the uncommon
retry explicit without a speculative clone.

## Consequences

Claim clones remain valid because they share the same immutable resident
identity. Foreign issuers and stale live claims fail even if their visible
values match. Normal publication performs no Header-content comparison and no
body clone; `FlushRequired` preserves the original body values for exactly one
flush-and-retry loop. Checkpoint fingerprints and every durable wire remain
unchanged.
