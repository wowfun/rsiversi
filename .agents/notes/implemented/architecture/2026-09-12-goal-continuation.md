---
name: Goal state and live continuation authority
---

## Problem

An ordinary Turn has finite elapsed, provider, Tool and generated-record
budgets. Repeating it requires an explicit owner of aggregate rounds, failure
policy and human priority. Agent composition commands have no Kernel mutation
service; putting a driver inside a domain callback would cross that boundary.
Mailbox acceptance and claim already have separate durable identities and
receipts.

## Decision

An Agent Goal plugin owns its domain, pure commands, context/projection and a
model report Tool. A Host Goal controller owns one bounded live lease per
Session and uses Kernel continuation admission. A deterministic message is
reserved by an internal command/CAS, charging the round immediately, then
accepted through a Kernel-issued live authority. Claim keeps its existing atomic
activation, Turn, Step and input commit. Continuation command provenance freezes
reservation data; ordinary command endpoints cannot dispatch internal
operations.

Ordinary waking input discards unclaimed automatic input in the same admission
transaction. Claim rechecks live authority and domain revision. Pause revokes
before discarding pending input; Cancel additionally targets the exact automatic
message/Turn. Startup claim admission rejects absent leases before any driver
can start. Restart is disarmed; explicit Resume preserves consumed rounds.

Every ordinary request receives the latest Goal and remaining round count. The
model can only report completion, blockage or a pause request. Its report Tool
has no Kernel service: a pure post-Tool contribution validates the actual named
Tool intent/result and records the claim with its source Turn. The Host settles
completion only after a successful canonical Turn outcome. Failed, partial,
interrupted and exhausted Turns block first; cancellation pauses. Unknown Store
or receipt outcomes disarm without inventing a durable transition or a new ID.

## Alternatives considered

A claim-time domain mutation would add an unnecessary transaction seam. Existing
command receipts plus frozen message identity support reserve/accept
reconciliation. Auto-arming from history or projection would resume work after
restart without live user intent. Reusing message cancellation for Pause could
cancel an already claimed Turn, so pending-only discard is a separate operation.
A global money or subagent-tree budget needs accounting outside this milestone's
parent-Turn cap.

## Consequences

Ordinary input may publish a staged Goal before its first automatic input wins
admission. Explicit Resume binds that same baseline reservation to a durable
internal reserve receipt, preserving its charged round and frozen message.
Broadening baseline admission to existing Sessions would weaken the Kernel
provenance boundary; inventing a discarded receipt for an absent mailbox input
would falsely assert a canonical disposition. The ordinary internal command
path supplies the missing durable provenance without either change.
Explicit Cancel can instead abandon a never-accepted allocation after revocation
and a serialized absence read. This is a Goal-domain settlement, not a claim of
mailbox discard, and preserves the charged count while permitting replacement.
Post-Tool revision conflicts drop the stale proposal without replaying callbacks;
an unrelated user command must not turn a retained Tool result into an internal
execution failure. The [domain commit decision](2026-09-08-typed-domain-commits.md)
continues to own atomic publication and uncertain-outcome semantics.

The [Agent Goal contract](../../../../crates/rsi-agent/goal/README.md) owns pure
state; the [Host Goal contract](../../../../crates/rsi/goal/README.md) owns live
driving; the [Kernel contract](../../../../crates/rsi-agent/kernel/README.md)
owns admission authority.

Kernel admission tests exercise reservation provenance, exact retries, human
preemption, forged sources/internal commands, startup and generation withdrawal.
Actual Session tests cover draft admission, Pause/Cancel, report authentication
and canonical settlement; controlled Host tests cover lost command replies,
receipt-read failures, dropped waiters and control timeout without replay. Agent
state and Session tests establish terminal precedence, completion-claim
provenance and no refund/reset. GUI detach leaves the Host owner running; Host
teardown revokes and joins it. Draft first admission publishes Header and staged
baseline together.

Reservations count even if never claimed, making the bound conservative and
auditable. Multiplying explicit max_rounds by the frozen five-dimensional budget
is an automatic parent-Turn allowance, not monetary, token or full-tree cost.
Model completion remains a claim until independent task acceptance succeeds.

A pause/cancel revokes before checking the durable predecessor. A known revision
or identity conflict remains a typed Session command rejection with its captured
binding, so the caller can issue a fresh explicit control. Mapping that
rejection to a backend failure would falsely make its outcome unknown on Session
API and trap the caller in receipt reconciliation. Revocation alone does not
claim that the durable phase changed or the active Turn was cancelled.

Client control feedback retains a known rejection independently of live and
durable observations. Those streams can arrive in either order, so an observed
Disarmed driver does not establish that a displayed control includes the latest
settlement revision. Rebasing a displayed control automatically was rejected:
the same Goal identity can already name another round, especially for Cancel.
A new explicit action uses the newly displayed snapshot; an unknown action
keeps its original identity for receipt reconciliation.
