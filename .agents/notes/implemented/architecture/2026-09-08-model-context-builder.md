---
name: Model context builders share the exact Agent generation pin
---

## Problem

Direct ContextFold construction in Executor and checkpoint maintenance prevents
ordinary Agent contributions from replacing context construction. A second
selection path for maintenance could also encode a cache under a different
generation than the Session's execution policy.

## Decision

The existing deep Context module owns the context interface, cache envelope and
default provider. The selected Agent Profile publishes an ordinary Local
ModelContextBuilder capability. Composition isolates that capability, requires
exactly one provider when sealing the unpublished generation, and freezes it
beside the Tool Runtime in AgentCompositionPin. Both execution and delayed
maintenance use the captured pin.

Builders are synchronous and free of external effects. Typed page kinds retain
the existing distinction between claim-visible scans, canonical Facts and fork
seed history. A generic bounded Context-owned envelope binds the immutable
builder identity and config digest in addition to the existing Header, limits
and Fact-prefix bindings. Cache incompatibility triggers replay without changing
the authoritative Store format. The default provider wraps ContextFold without
changing workspace context or adding ephemeral time input.

## Alternatives considered

A new protocol package would separate the same narrow context vocabulary from
its only deep owning module without resolving an actual dependency cycle.
Keeping the executor's default fallback would hide a missing Profile selection.
Resolving the latest builder during maintenance would cross generation ownership
and let a superseded implementation rewrite another pin's cache.

## Consequences

Default requests and fork/claim behavior remain equivalent. A second provider
fixture proves both execution and maintenance use the chosen generation, including
after the catalog changes. Missing and duplicate providers fail composition and
reclaim the complete candidate. Cache tests reject changed identity, config,
limits, Header, position and payload, preserve state after failed restore, and
retain single-slot Store behavior.

The default payload retains its existing private fold encoding beneath the new
generic envelope. This small metadata overhead can make a previously maximal
cache ineligible, which only causes bounded replay. Synchronous in-process
builders are trusted bounded computation; arbitrary blocking plugin code cannot
be preempted by an async cancellation token.
