# Composition generations

Before first submission, an `AgentSessionDraft` owns the candidate header and
one exact composition pin without creating Store state, reserving Kernel
capacity, or registering the candidate Workspace. Changing its preset fully constructs and validates the replacement
generation before atomically exchanging the draft's identity and pin. Consuming
the draft yields one move-only fresh-session value; after that ownership
transfer no switching interface exists. Failure or dropping an unsubmitted
draft leaves no durable session.

Agent composition resolves one preset source digest into a standing child Scope
inside the existing Runtime. It starts an unpublished Tool catalog stage,
activates the preset's allowlisted contribution Profile, requires every child
Fiber to become Active, requires one explicitly selected context builder, seals
the exact Tool and typed domain catalogs, and only then publishes the
generation. Candidate failure disposes the complete stage and never replaces a
healthy current generation. Construction is single-flight per preset identity; reuse requires the compiled
source and the exact factory/marker/isolation catalog identity. Each build obtains
one application-owned immutable snapshot of the preset compiler and contributions
before compilation. Existing pins retain their old generation across publication. A superseded generation remains alive while a draft,
resident session, or admitted Tool result holds its pin, then tears down after
the final pin releases. Domain registrations use exact Meta registration credentials
and leases. Rollback and sealing close the unpublished registrar; frozen pins
retain the validated definitions and bounded initial states.

Within each selected snapshot, the preset catalog and generation builder share
one application-supplied frozen Profile compiler. Fresh roster discovery compiles each winning source, including
required includes and pure expressions, checks enabled contribution identities
against the frozen Agent-only allowlist, and keeps failed rows visible with a
bounded categorical diagnostic. The roster receives neither concrete factories
nor the Host catalog. That health is observational only: generation selection
probes the exact preset id in root-precedence order without compiling unrelated
roster rows, compiles that selected source once, and then resolves it against
the Agent-only factory allowlist before any Runtime mutation. The catalog
neither receives nor exposes the Host factory catalog.

Resume validates the proposed turn body against the durable header before an
idle historical session is admitted to resident Kernel state. Invalid requests
therefore cannot consume the active-session bound. Claim reads return a Store
page before consulting the speculative suffix whenever the durable watermark
advances during Store I/O, preserving one contiguous prefix across races.
Resume preparation is a move-only admission step at the Turn-service boundary.
It returns the authoritative Header together with either the resident session's
exact pin or the current healthy generation for a cold session. Applications
must complete this preparation before creating any durable workspace
registration or other run-local side effect, and submission consumes the token.
A missing or broken cold preset, unsupported domain codec, missing frozen domain
state or invalid typed payload therefore fails before workspace mutation,
resident capacity, Fact materialization, or external effects. A resident
session continues using its existing pin across source changes; after idle
eviction or process restart, preparation deliberately acquires the latest
generation for the same durable preset identity. Dropping an unsubmitted token
releases its pin and has no Store or workspace semantics.

The six native Agent-control Tools are thin adapters over the Turn service and
receive caller authority only through the generic typed Tool execution extension
slot. Presets are bounded Profile sources for allowlisted Agent-plane
contributions; provider factories remain in the global Profile. Composition
uses ordinary child Fibers in the same Runtime and does not provide a second
runtime, privileged Host catalog, or hidden service locator.
