---
name: Stable composition positions for ordered contributions
---

## Problem

Registration timing determines Local listener, approval-answerer, and finalizer
precedence. Selective Profile rebuilding would change this order.

## Proposal

Separate a member's stable generation-owned position from its current rank.
Context-derived registration credentials bind contributions to their exact owner
and position. Loading registration joins setup rollback; Active registration
uses an effect with publication guarded against retirement. Portable callback
effects retain their distinct invocation lifetime.

Consumers capture immutable membership/order snapshots. Prepend uses reverse
declaration order before the forward append lane; once remains atomic. Approval
uses this order for its first answer. Finalizers remain concurrent and use order
only to resolve results. Tool/Command name indexes and reverse effect cleanup
keep their contracts. Core remains usable through existing Execution in Workers.

## Alternatives considered

Registration timestamps do not survive selective rebuilds. Suffix rebuilding
retires unrelated effects. Cordis motivates selective updates but does not
provide this stable-order design. A second Runtime cannot transfer arbitrary
effects atomically.

## Acceptance criteria

Public tests cover replacement, reorder, prepend, once, nested Fibers, Loading
rollback, Active retirement, exact capacity, independent Profiles, and real
Chromium/Firefox Workers. Unchanged dispatches reuse bounded snapshots.

## Risks

Prepend becomes declaration-relative. Mutable rank must not become a map key.
Caches observe membership and order revisions. Rollback restores only its own
subtree, and failed order gates prevent enabling exact convergence.
