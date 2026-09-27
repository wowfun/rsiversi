---
name: Composer delivery preferences and migration
---

## Problem

The old required Enter boolean cannot distinguish an intentional false setting
from a default persisted by a form. Browser-side busy inference can also change
the meaning of a displayed action before preparation.

## Decision

The product uses Enter sending and Queue while busy as the explicit new defaults. Owner registration migrates
both old boolean values once at owner registration, preserving appearance. It binds
Rust-projected delivery actions to an acknowledged presentation revision, then
freezes the chosen request in the existing ledger. Direct Steer keeps its existing
current-step-or-next-Turn semantics; only queued conversion and Stop bind a Turn.

## Alternatives considered

Preserving false would retain an accidental old default for most clients. Direct
GUI provider writes would bypass registration's raw cache and CAS ownership.
Changing Agent delivery to exact-Turn Steer would require an unrelated durable
format change. Deriving actions in JavaScript would duplicate Rust state policy.

## Consequences

Legacy values and theme-only documents migrate without resetting new values.
Migration failure publishes no owner. Keyboard and touch actions agree, stale
actions preserve drafts, and ledger retries preserve the chosen delivery.


Registration now has an asynchronous persistence phase. It must reserve the
namespace and finish cache convergence independently of caller cancellation.
Concurrent Hosts can migrate the same raw section. A bounded provider-CAS retry
reloads that section and reruns the pure migration, accepting an already migrated
value without another write. This avoids turning an idempotent startup migration
into a permanent activation failure after a single competing publication.
