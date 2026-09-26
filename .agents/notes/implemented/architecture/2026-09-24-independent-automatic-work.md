---
name: Independent automatic owners with atomic idle admission
---

## Problem

A continuation registry with one slot for the entire Session excludes an
independent reminder owner; reserving rounds before mailbox admission also charges
work that cannot start. Adding
reminders to that registry would either evict Goals or charge busy rounds.
Detached programs also need an idle admission boundary distinct from their
creator Turn's execution claim.

## Decision

This partially supersedes the [Goal continuation decision](../../implemented/architecture/2026-09-12-goal-continuation.md).
The Kernel retains continuation owners by `(Session, domain)`, bounded to 64 per domain and
128 overall, with identical Header and generation for co-resident owners. It permits
one automatic Turn per Session, gives human input priority and alternates eligible
owners. Allocation and exact MessageAccepted commit together under idle admission;
Busy neither charges nor revokes the live owner. There is no separate submit
phase: command receipt reconciliation uses execute/query and accepted input uses
message_status. Removing the obsolete receipt-only submit seam avoids a second
authority path whose name implies admission. The Goal first round follows the
same rule, while report_goal retains exact reservation authentication. Waiters
observe peer revocation without retaining peer authority; dropping the last owner
wakes selection. A transaction overlapping revocation still returns its canonical
receipt, while the revoked lease remains ineligible for subsequent claims.

Schedule owns bounded timer intent and total rounds, independently of Node.
ProgramRun execution ownership is distinct from fork lineage and from a retired
creator claim. Both restart disarmed/interrupted and never replay external work.

Continuation leases survive known precommit capacity and command-revision
contention. Schedule retries with fresh state and bounded exponential pacing;
unknown Store outcomes remain non-replayable. Initial acceptance classification
is shared with workflow admission, follows exact accepted message identities
across pages, and excludes later steering. Root and allowed-domain policy remain
with the calling adapter. Schedule reads plan-policy again after disarming its
prior driver and guards that revision under Kernel commit admission. A second
preflight read alone would still leave a check-to-commit race; a read-only guard
avoids manufacturing a policy write that would revoke unrelated Programs.

## Alternatives considered

Adding another ad hoc timer or process loop would bypass existing owner and
recovery fences. A second continuation slot without atomic admission retains
busy charging. Treating fork ancestry as execution lifetime prevents a detached
run from completing after its originating Turn has ended.

## Consequences

This extends durable provenance and cold-state validation. Store layouts must
reject incompatible old schema versions explicitly; there are no compatibility
shims. Local Node execution remains shell-equivalent, with no VM sandbox claim.

Public admission races prove human priority, no busy charge/revoke, independent
owner lifetimes and bounded aggregate work. Repeated restart cannot arm work.
Program completion has an exclusive run sink; it cannot leak child results into
ordinary parent mailboxes or chain new runs without fresh authority.
