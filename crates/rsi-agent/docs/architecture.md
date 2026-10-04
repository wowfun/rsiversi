# rsi-agent architecture

Agent is a composition of ordinary process-local plugins. No package-wide
adapter owns their plugin identity. The SQLite Store, Kernel, and executor each
export their own factory; the product composition root assigns stable plugin
and instance identities and places them in a Profile.
Composition roots must install a service-level `ContextBudgetContract` before
activating the executor; every Agent Scope inherits that same authority. The
preset fragment supplies its instance entry using the factory registered by the
product composition root, while custom roots must provide it
explicitly or executor activation fails its required Local dependency. The
[Context contract](../context/README.md) owns its admission and retention rules.

The durable boundary is deliberately narrower than the runtime boundary.
`rsi-agent-session-protocol` owns validated session identities, the immutable
session header, and append-only Facts. `rsi-agent-store-protocol` owns only the
mechanical persistence seam and its compare-and-append rules.
`rsi-agent-turn-protocol` owns the process-local submit, cancel, observation,
and outcome service. Runtime scheduling and recovery policy belong to the
Kernel, with their shared boundary described in [Sessions and recovery](subsystems/sessions-and-recovery.md).
[Turns and Agent trees](subsystems/turns-and-agent-trees.md) explains execution
claims, mailbox inputs and parent/child lifetimes.
Model prompt projection and compaction belong to
`rsi-agent-context`, not to the executor or Store; [context inputs](subsystems/context-inputs.md)
explains capture, selection and projection ownership.
The [Goal domain](../goal/README.md) composes pure allocation, reporting and
settlement callbacks over these contracts. Its scheduling owner belongs to the
standard Host; Kernel authenticates the separate continuation lease, reservation
receipt and mailbox input. The [model selection domain](../model-selection/README.md)
owns durable Session model/effort changes; each new execution Step captures that
selection. The [Todo domain](../todo/README.md) owns the bounded task list committed
atomically with its Tool result. Both are Agent-only contributions over the
existing domain catalog, not new scheduling or storage owners.
Kernel's read-only Jobs port consumes public Jobs
status types and validates an executor-published source against the live claim;
it cannot create or own the underlying Jobs scope. [Program runs](subsystems/program-runs.md)
describes how durable workflow transitions bind to that execution lifetime.
[Automatic work](subsystems/automatic-work.md) separates durable continuation
state from live scheduling authority. [Composition generations](subsystems/composition-generations.md)
explains how immutable preset contributions stay pinned across execution.

```text
SQLite Store --Local--> Kernel --Local Turn service--> callers
                            ^
                            |
                    executor registration and claims
                            |
            Agent composition pin -> immutable Tools and context builder
                            |
          Preset Profile contributions over global providers
```
