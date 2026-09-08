# rsi-agent-composition-protocol

This package owns the process-local interface between Agent composition,
session drafts, Kernel, and Executor. An `AgentCompositionPin` carries one
validated preset identity, one exact Profile source digest, one immutable Tool
Runtime, one immutable ModelContextBuilder, frozen typed domain definitions, and opaque lifetime ownership for
the standing generation. Execution and delayed checkpoint maintenance retain
that same pin; maintenance never resolves a newer builder independently. Consumers
cannot obtain a registrar, Profile resolver, Scope, or provider catalog from a
pin.

`AgentComposition` reports the effective default identity and resolves the
current healthy generation for one durable preset identity. Both operations
come from the same standing catalog authority; callers receive neither that
catalog nor its Settings adapter. `AgentSessionDraft` is move-only and
process-local: switching first acquires a complete replacement pin, then
atomically replaces the draft header and pin. Consuming the draft produces
`PreparedFreshSession`; dropping a draft has no persistence semantics. Kernel
owns the transfer of this value into resident session state and resolves cold
resumes through `AgentComposition`.

Drafts retain actual typed domain initial state. Applying an initial proposal
changes that payload only; a successful preset switch resets it to the selected
generation's defaults, and a failed switch preserves it. Fresh admission moves
the Header, pin and complete baseline together. Its digest distinguishes frozen
initial states during first-submission retries. A lease-owning Session adapter
may freeze an admission while retaining the draft for failed-submission retries;
it serializes freezing, initial-state changes and first publication under one
draft mutation admission. Freezing preserves the complete baseline and exact
generation. An initial-state batch is validated as a whole and either replaces
all requested domains or leaves the draft intact. Empty initial state has no
baseline control and uses the documented zero digest.

This package contains no preset filesystem discovery, generation construction,
Store access, Profile catalog, executor loop, or CLI behavior.

## Typed domain proposals

`DomainDefinition<T>` owns one exact domain identity/version, a validated initial
state, and the pure bounded validator for `T`. Encoding passes through the
Session protocol's bounded complete-state value before a proposal can exist.
`DomainCatalog` freezes unique definitions and bounds their combined baseline.
Binding a definition produces a typed handle for that exact catalog generation;
a proposal from another catalog is rejected even when its public identities
match. A proposal carries expected revision and complete replacement state, but
no Store writer, Session authority, request identity, or charging category.
Those belong to Kernel mutation admission.

`DomainRegistrar` accepts exact `RegistrationContext` credentials while the
composition stage is unpublished. Each binding belongs to one registration and
generation; its lease withdraws only that entry. Sealing excludes registrations
whose admission already closed and closes the registrar. Frozen pins keep their
definitions independently of later lease cleanup. Candidate rollback also closes
the registrar, including copies retained by failed plugins. The pure
`DomainCatalogBuilder` supplies this adapter's bounded data model, not a second
lifecycle owner.

Reading an opaque state does not require its codec. Typed decode and execution
validation require the exact declared domain version and run its validator;
there is no compatibility fallback. Validators and serializers are synchronous
linked code: they must remain bounded and perform no I/O or external effects.
The framework bounds encoded output but cannot preempt a blocking callback.

## Execution contributions

The unpublished composition stage also freezes context contributors, post-Tool
contributors and Tool policies. One shared registrar owns exact Meta
registration cleanup. Sealing filters already closed registrations, captures
one composition-order revision, and orders by ascending priority, composition
position and ContributionId. Owner-local registration timing is not the final
tie-break. The immutable catalog belongs to the same generation pin as Tools,
domains and the context builder; a failed candidate publishes none of them.

Callbacks receive an immutable Fact/control horizon, complete bounded domain
states and a claim-scoped read-only Fact reader. They have no Store writer or
Kernel mutation service. Context and post-Tool callbacks return bounded input
messages and exact-generation domain proposals; the executor validates the
whole stage output before its single business commit. Ordinary context input
receives the registered producer's PluginContext source. Explicit workspace
instruction/catalog/invocation sources retain their closed protocol roles;
callbacks cannot manufacture Human, Agent or Completion transport messages.

A stage returns at most 512 inputs and 16 MiB of complete input Fact envelopes,
alongside the Session protocol's bounded domain mutation set. Readers use the
existing bounded Fact page contract. No callback runs while a registry,
submission or lifecycle lock is held. Execution owns cancellation and a bounded
stage deadline, and records the failing producer and stage in its diagnostic.

Tool policies return only Abstain, RequireApproval or a bounded Deny reason.
Deny takes precedence, and no policy can relax resolved Turn approval or
Sandbox requirements. Policies inspect the exact prepared Tool call and the
same pinned domain snapshot; they do not produce state mutations.

## Command contributions

Session command callbacks are frozen by the same unpublished registrar and
generation as execution contributions. Each has a bounded description, an
explicit draft-safe flag and a unique command name. Callbacks receive the
immutable Header, typed revision and complete bounded domain snapshot, and
return only typed domain replacements. They have no external-effect or Store
capability. The owning Session or Kernel validates and atomically commits the
complete output after the callback finishes outside framework locks. Duplicate
request lookup precedes callback invocation and revision checks. A conflict
never causes an automatic callback retry.

Draft command preparation captures the lease-local revision, actual initial
states and exact callback generation. Execution returns a move-only proposal
batch; applying it requires the same draft identity and unchanged revision.
The Session lease owner serializes that final application with first submit and
preset selection, while the callback runs without holding that admission.
Identical request lookup precedes preparation. A draft retains at most 256
compact command receipts for its lifetime and rejects further distinct command
mutations at capacity; it never evicts an idempotency receipt and later silently
re-executes the same request. Preset selection resets initial defaults, advances
the draft revision and preserves earlier receipts as results of their original
operations. They do not claim to describe the draft's latest state.
Preset preparation also returns an owned future and a move-only staged result,
so plugin generation construction runs outside the Session mutation lock. Final
selection checks the same draft identity and predecessor before replacing the
Header, pin and defaults together.

## Extension projections

Projection callbacks enter the same registrar and immutable generation as other
Agent contributions. A framework adapter supplies the exact Header, a draft
revision or simultaneous durable Fact/control horizon, and the complete bounded
domain set at that cut. Callbacks derive a whole JSON view from those captured
values; they receive no mutation or external-effect capability and do not own
subscriptions. Core conversation history remains independently readable.

The adapter attributes each value to its registered producer and validates it
before publishing a complete snapshot. Failure, panic or oversized output from
one callback becomes only that producer's bounded failure entry. Cooperative
callbacks have a one-second individual deadline within a 30-second whole-capture
deadline; cancellation drops unfinished callbacks. As with other linked Rust
callbacks, a blocking poll cannot be preempted. All callbacks run outside
registration, Session mutation and lifecycle locks. Projection values are
derived and never written back as domain state or used as mutation authority.
