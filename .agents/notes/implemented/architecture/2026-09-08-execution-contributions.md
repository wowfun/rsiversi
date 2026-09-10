---
name: Ordered execution contributions preserve durable input and policy provenance
---

## Problem

Workspace interpretation inside Kernel prevents built-in behavior from using
the same extension paths as addons. Sampling context inside a model builder
loses replay provenance, and policy denial cannot truthfully be represented as
an executed Tool result. Callback registration also needs one generation and
one stable order across asynchronous activation and reload.

## Decision

The unpublished Agent generation freezes its contribution catalog together
with Tools, domains and the context builder. Entries use priority, Meta
composition position, then ContributionId. Meta exposes positional comparison
without replacing its ordinary owner-local registration ordering. Exact Local
isolation keeps candidate registrars separate from retained Session pins.

The [composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns callback inputs, output bounds and frozen identities. The executor runs
each stage outside framework locks against one durable Fact/control snapshot.
It validates all outputs before committing any business output, revokes its
captured reader before mutation, and never repeats callbacks to reconcile an
uncertain commit. Provider retries reuse the already committed context sample.

PluginContext records actual model input and its producer. ToolRejected records
the exact prepared call and either approval or policy denial, without an
execution start. Rejections still consume Tool-call and generated-record
budgets. Approval requirements accumulate; a policy cannot relax the resolved
Turn policy. Post-Tool callbacks receive the settled provider batch in source
order after all its scheduling groups complete.

Workspace instructions, skill catalog and human skill invocation are ordinary
contributors. Their digest, last-good and Session cursor state uses the typed
domain substrate, so it commits with entered input. Kernel and Store no longer
own a workspace digest index. The [workspace contract](../../../../crates/rsi-agent/workspace-context/README.md)
owns incomplete reads, replacement, tombstones and fork cursor rebinding.
Time context samples UTC before a model request and persists that exact text.

## Alternatives considered

Keeping a special Kernel hook would leave two ownership paths. Injecting time
only into the builder would make retries and replay differ. Reusing registration
timing as a business tie-break would make activation order observable. Fake
Tool results on rejection would imply effects that never ran. A second workspace
cache in Store would duplicate authority already held by the domain stream.

## Consequences

The schema-15/Header-10 cutover admits the new authoritative vocabulary and
removes obsolete workspace index columns; previous databases remain untouched.
Cold recovery validates current codecs without rerunning historical callbacks.
Forked workspace cursors rebind to child history while retaining the selected
parent state. Initial empty sources emit no fictitious tombstone.

The asynchronous stage deadline bounds cooperative callbacks and closes readers
on cancellation, panic or failure. Trusted linked code that blocks its executor
thread cannot be preempted by a timer. UTC sampling provides deterministic
provenance; browser timezone preferences are outside this contributor's current
contract. Public composition, Kernel, Store and executor tests cover ordering,
rollback, atomic output, ending admission, retry, denial, cold recovery and fork.
