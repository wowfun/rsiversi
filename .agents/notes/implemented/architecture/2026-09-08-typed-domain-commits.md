---
name: Typed domain commits share Session authority and generated-record budgets
---

## Problem

Plugin-owned state needs durable revision and retry semantics without granting
plugins an arbitrary Store writer. Fact-only budgets omit control-only work,
and exhausting a budget with an open Step must not make ending the Turn
impossible. Uncertain Store acknowledgements cannot safely become permission to
repeat effects or continue from stale resident state.

## Decision

Typed definitions and pure validators enter the same unpublished generation as
Tools and the context builder. Frozen handles retain exact registration
identity; matching public names do not authorize a replacement generation.
The [composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns definition, baseline and proposal semantics.

One canonical domain control contains every replacement in a bounded request.
Its digest binds the Kernel-assigned source, replacements and accompanying
Fact bodies, independent of allocated record positions and timestamps. Store
indexes retain positions, update offsets and mechanically verified accounting
metadata; they do not retain another authoritative state payload. Plugin
validators run before Store admission and never under its locks.

Only a fresh baseline creates domains. The actual prepared state joins Header
and first acceptance atomically. Fork baselines use the selected terminal's
exact control horizon, overlaying supported historical values on target
defaults. Cold execution requires complete codec support; raw history remains
readable without those codecs. These choices follow the single-definition and
post-durability publication lessons in the local DeepSeek Harness reference,
while retaining RSI's sole Session control authority and explicit cutovers.

The [Kernel contract](../../../../crates/rsi-agent/kernel/README.md) owns mixed
mutation admission, retained ownership and result reconciliation. All Facts
and Turn-origin domain controls are charged together before either stream
changes. Canonical full envelopes determine byte usage. Baselines and external
commands have separate bounded admission; producers cannot choose a free lane.

Kernel ending permits only a bounded final suffix: the current Step closure,
a necessary budget marker and the terminal. Only that adjacent final closure
escapes generated-record and elapsed limits. Business batches cannot insert
ending records. The [terminal correlation decision](2026-09-08-terminal-control-boundaries.md)
continues to own the same-transaction Fact/control boundary.

Once ending is admitted, failure does not reopen business work. A proven
precommit failure permits settlement retry. If both commit result and canonical
lookup are unknown, the exact mutation gate becomes Failed and the Session
requires cold recovery. Explicit claim release and read-only result queries
remain available. Dropping a caller's waiter never abandons an admitted task.

## Alternatives considered

Arbitrary JSON writers or a separate domain Store create a second authority.
Independent per-domain receipts expose partial requests. Recomputing a request
against newer revisions changes its identity rather than reconciling it.
Implicit codec defaults on cold resume conceal missing persisted state.
Sampling today's control tail for a fork includes post-terminal commands.

A reusable persisted free-Step flag would enlarge the uncharged lane.
Closing a Step in a separate transaction can strand it before terminal
settlement. Immediately retiring an uncertain claim breaks explicit release;
reopening it permits business operations with unproven revisions and budgets.

## Consequences

Schema 14 and Header format 9 replace the previously published 13 and 8 by
explicit rejection, preserving old databases. Existing generated-fact limit
names are replaced throughout their consumers without compatibility aliases.

Shared Memory/SQLite contracts cover complete-state CAS, exact mixed Fact
binding, historical lookup and usage. Cold and offline SQLite checks reject
canonical/index corruption. Kernel fault tests cover both sides of Store apply,
cancelled waiters, unknown receipts, terminal acknowledgement loss, settlement
retry, queued-Turn wakeup, exhausted open Steps and read-only recovery queries.
A production Kernel-to-SQLite test reopens under a new codec generation and
verifies further commits plus offline audit.

The [contribution decision](2026-09-08-agent-domain-state.md)
owns PluginContext, ToolRejected, contribution execution and command/projection
integration. This substrate does not make those consumers available by itself.
