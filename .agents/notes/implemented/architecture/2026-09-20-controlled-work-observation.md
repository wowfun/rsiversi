---
name: Controlled work settlement is independent of durable Turn outcome
---

## Problem

The Executor retires retained Tool results after terminal durability. Consumers
that treat terminal observation as completion of all effects can return a cancel
response or capture a workspace diff while controlled work is still running.

## Decision

Executor counts separate guards for the claim drive and every tracked Tool.
Successful finalization and confirmed release of every guard publish Settled;
unconfirmed drop or failed finalization publish Unsettled. Kernel authenticates
the observation against the exact claim and retains at most 1024 entries,
evicting only non-running entries. Observers retain their original generation.
The [Turn protocol](../../../../crates/rsi-agent/turn-protocol/README.md) owns
the current public contract and its explicit unknown/cold-history behavior.

An interval reserves bounded observation capacity before execution-lane admission.
Baseline capture remains before effects; final evidence runs after the claim drive
without retaining its execution lane. Admission and baseline share a deadline,
as do controlled-work observation and final capture. This prevents slow evidence
from serializing completed and subsequent Turns while retaining finite ownership
and a joined shutdown. Observer admission is asynchronous and cancellation-aware;
implementations must still avoid blocking a future's polling thread.

## Alternatives considered

Waiting for the existing durable outcome does not join retirement tasks.
Extending terminal persistence to wait for arbitrary Tools would destroy bounded
Turn completion. Adding a second finalizer scheduler would duplicate the existing
Executor deadline and effect-owner cleanup policy.

## Consequences

Final capture may overlap a later Turn once the execution lane is released.
Together with independent editors and processes, this prevents attributing every
changed line to one Turn. The observation describes its actual capture interval;
it does not promise exclusive workspace ownership or an atomic terminal snapshot.

No Session or Store schema changes are needed. Consumers must impose their own
deadline and preserve unknown/unsettled outcomes. Real Executor tests hold an
admitted Tool past elapsed-budget terminal, then prove that releasing it is what
changes Running to Settled. Finalizer timeout and stale claim publication have
separate negative regressions. Optional checkpoint work is outside this proof.
