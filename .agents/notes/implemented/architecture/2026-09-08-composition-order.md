---
name: Stable composition positions for ordered contributions
---

## Problem

Registration timing determines Local listener, approval-answerer, and finalizer
precedence. Selective Profile rebuilding would change this order.

## Decision

A member's stable generation-owned position is separate from its current rank.
Context-derived registration credentials bind contributions to their exact owner
and position. Loading registration joins setup rollback; Active registration
uses an effect with publication guarded against retirement. Portable callback
effects retain their distinct invocation lifetime.

Consumers capture immutable membership/order snapshots. Prepend uses reverse
declaration order before the forward append lane; once remains atomic. Approval
uses this order for its first answer. Finalizers remain concurrent and use order
only to resolve results. Tool/Command name indexes and reverse effect cleanup
keep their contracts. Core remains usable through existing Execution in Workers.

Position metadata has its own explicit Runtime bound. Candidate positions can
coexist with current positions without reserving duplicate Fibers, and retained
metadata owns neither a Runtime nor execution admission. Final ancestor cleanup
is iterative. Runtime identity is opaque process-local metadata, separate from
the live registration credential.

Local protocols borrow the core-owned credential rather than introducing a
second owner/generation token. Exact registration cleanup shares the existing
Local listener removal mechanism, including publication/retirement exclusion,
bounded cleanup failure evidence and weak Runtime retention in a returned lease.

Scope adds an explicit bounded `ScopedContributions` table for ordered hooks.
It applies global then ancestor layers, with declaration order inside each
layer. Existing named overlay tables retain their own mutation and notification
contract. The ordered table caches only its last selection so reads cannot build
an unbounded history of queried scope keys.

## Alternatives considered

Registration timestamps do not survive selective rebuilds. Suffix rebuilding
retires unrelated effects. Cordis motivates selective updates but does not
provide this stable-order design. A second Runtime cannot transfer arbitrary
effects atomically.

## Consequences

Public tests cover replacement, reorder, prepend, once, nested Fibers, Loading
rollback, Active retirement, exact capacity, independent Profiles, and real
Chromium/Firefox Workers. Unchanged dispatches reuse bounded snapshots.

Prepend becomes declaration-relative. Mutable rank must not become a map key.
Caches observe membership and order revisions. Rollback restores only its own
subtree. Retained contributions can still react to Meta dependency convergence;
position identity does not promise cross-registry atomic publication.
