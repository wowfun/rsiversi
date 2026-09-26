---
name: Exact saved plans and atomic human handoff
---

## Problem

A prose answer must not silently authorize execution, and a review may outlive
the plan or mode it displayed. A separate review plugin cannot write another
plugin's typed domain because registration ownership is checked by identity.

## Decision

The [plan-policy owner](../../../../crates/rsi-agent/plan-policy/README.md)
owns saved plans and the review domain. Its settlement callback holds both
handles and commits approval, mode exit and Tool result atomically. The
closed choice binds to the plan reference and both observed revisions. Generic
human questions remain product-neutral; all native clients send typed choices.
The [prior plan decision](../../implemented/feature/2026-09-09-plan-policy.md)
remains authoritative for policy; this extends its one-domain composition.

DSH's `packages/plan/plan-mode/src/index.ts` provides a complete-plan review and
checks the selected approval label. RSI keeps its enforced allowlist and adds
durable revision fencing because live selection alone cannot authorize an
atomic state change across concurrent commands.

## Alternatives considered

An independent review plugin cannot propose the bool domain it does not own.
Recognizing approval from arbitrary text is ambiguous. Exiting mode before the
result commit leaves approval and execution authority inconsistent after failure.

## Consequences

A stale answer terminates its Turn through the existing failed-settlement path;
it is not a successfully committed approval. The broker receipt records delivery
only. The new domain intentionally changes generation support without migration.

Public execution tests cover approval, change requests, decline and stale mode
versions. All native clients render the exact plan and closed choices; ACP omits
these Tools. Cancellation and recovery cannot replay approval.
