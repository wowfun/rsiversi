---
name: Immutable Agent preset generations
comment: Separate global effect providers from session-pinned Tool catalogs
---

## Problem

A process-global mutable Tool registry cannot prove that model definitions,
Tool preparation, retained results, and durable session recovery refer to the
same composition. Grouping Bash, Jobs controls, and apply-patch in one coding
tools factory also gives unrelated capabilities one accidental lifetime and
makes a future shell implementation inherit Linux-specific ownership.

## Decision

Agent presets are bounded directories whose required `agent.profile.toml` is
preflighted for roster health with the same application-supplied frozen Profile
compiler used to build generations. This catches source, include, expression,
pure semantic, and unknown Agent-contribution failures using a read-only
identity allowlist without exposing the Host catalog or concrete factories;
roster diagnostics are bounded categorical text and the broken winning row
remains visible. The current source is then compiled again and resolved against
a construction-time frozen allowlist of Agent contribution factories, so
point-in-time roster health never authorizes a generation. A standing
composition provider creates a hidden Scope and one unpublished Tool catalog
stage, activates a private write-only registrar and the complete static Profile,
seals the catalog, and only then atomically publishes the generation. Resolve,
prepare, Pending, cancellation, activation, or sealing failure disposes the
unpublished Scope and never falls back to an older generation for the requested
current source.

Selected generation resolution walks only the exact preset id through root
precedence; sibling discovery and health compilation happen only for an
explicit roster request. A catalog that loses its last owner cancels active
calls, removes its settled retained results, and discards late settlements.
Query and commit authority therefore ends with the catalog, while an active
Tool body continues to own process-wide settlement admission until it truly
settles; withdrawing authority cannot manufacture quiescence or recycle a slot
still used by trusted code.

Fresh session drafts are move-only and carry an exact generation pin. The
durable Header records a separate required preset id. A resident Kernel session
retains its original pin; a new session or cold resume resolves the current
source generation. Claims expose that resident pin to the executor, and any
retained Tool invocation keeps a clone until settlement. A superseded hidden
Scope is disposed asynchronously only after its final current-generation,
session, draft, or Tool pin is released.

Bash process production is global because a complete `ProcessSpec` crosses the
producer seam and Jobs may outlive the Agent generation that submitted them.
The Bash definition, generic Jobs controls, and apply-patch definition are
independent Agent contribution factories. There is no generic shell core until
a second concrete shell demonstrates shared public semantics, and there is no
generic filesystem provider around the single structured patch capability.

## Alternatives considered

Keeping a mutable global Tool Runtime was rejected because definitions and
prepare could observe different registry generations. Keeping
`rsi-coding-tools` was rejected because its only abstraction was simultaneous
registration of unrelated capabilities. A speculative common shell or
filesystem crate was rejected because the current implementation supplies no
second backend contract to abstract. Reusing the model Profile id as the Agent
preset id was rejected because provider routing and Agent capability ownership
change independently.

## Consequences

Preset source changes affect only later drafts, fresh sessions, and cold
resumes; resident work remains internally consistent. The process retains at
most 256 live generation rows and eight concurrent builds; a failed idle row is
evictable under pressure so untrusted missing identities cannot permanently
consume admission for a healthy preset.
Catalog result capacity remains process-wide even though definitions are
generation-local. Built-in assets and writable presets require separate
filesystem ownership, and deletion cannot be one transaction with a Settings
override; the authoring API therefore preserves the preset directory when
clearing its selected default fails and reports later filesystem cleanup
failure explicitly.
