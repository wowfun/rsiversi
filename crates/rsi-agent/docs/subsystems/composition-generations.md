# Composition generations

Capacity refusal preserves the current generation and remains retryable at the
next settled boundary. A preparation deadline or source failure before candidate
identity is known defers preparation until the next Turn. Deterministic candidate
rejection is memoized for an unchanged selection and Domain cut within the same
Turn. Preparation has a 30-second budget; Context replay uses cancellable bounded pages without an aggregate history deadline;
publication retains the Store's owned commit/reconciliation lifetime.

Before first submission, an `AgentSessionDraft` owns the candidate header and
one exact composition pin without creating Store state, reserving Kernel
capacity, or registering the candidate Workspace. Changing its preset fully constructs and validates the replacement
generation before atomically exchanging the draft's identity and pin. Consuming
the draft yields one move-only fresh-session value; after that ownership
transfer the Kernel owns subsequent executable selection. Failure or dropping an unsubmitted
draft leaves no durable session.

Agent composition resolves one preset source digest into a standing child Scope
inside the existing Runtime. It starts an unpublished Tool catalog stage,
activates the preset's allowlisted contribution Profile, requires every child
Fiber to become Active, requires one explicitly selected context builder, seals
the exact Tool and typed domain catalogs, and only then publishes the
generation. Candidate failure disposes the complete stage and never replaces a
healthy current generation. Construction is single-flight per preset identity; reuse requires the compiled
source and the exact factory/marker/isolation catalog identity.
Hot-update probes compare that process-local identity as well as the durable
selection; nominal Rust marker identities are never persisted. Each build obtains
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
It validates public and private retained state before returning the authoritative
Header together with a pin from the current plugin composition when cold. Applications
must complete this preparation before creating any durable workspace
registration or other run-local side effect, and submission consumes the token.
A missing or broken cold preset, unsupported domain codec, missing frozen domain
state or invalid typed payload therefore fails before workspace mutation,
resident capacity, Fact materialization, or external effects. A resident
session can adopt a fully prepared successor at a settled Step boundary. The
source-only preparation runs without Session submission admission. A changed
candidate flushes its captured Fact prefix without holding submission admission,
then acquires admission and revalidates its claim and predecessor before
capturing current state. It releases admission during build, ordered validation
and context loading. Cancellation and other submissions remain available during
preparation. Publication reacquires admission and compares the captured Fact and
Domain cuts before committing; a changed predecessor discards the candidate and
allows the next settled boundary to retry.
The Kernel commits its executable selection and validated current Domain state
before publishing the new pin. An admitted model request, retries, Tool batch,
and retained jobs keep their original generation. A failed candidate leaves the
committed selection and current execution intact. Projection subscribers receive
the bounded rejection diagnostic while the old generation continues; it is
transient status and is cleared on adoption or return to the selected source.
After idle eviction or process restart, preparation uses the current plugins,
validates retained state for still-declared Domains, and initializes newly declared
Domains. Removed Domains remain in history. Completed Tools are never replayed. Dropping an unsubmitted token
releases its pin and has no Store or workspace semantics.
Cold hydration rechecks the prepared Domain cut and loads Turn state before
publication. These reads remain cancellable; abandoning hydration settles its
waiting readers and releases admission. Once publication starts, its
owned task retains submission admission and the hydration guard through durable
reconciliation and resident installation, even if the requesting future is dropped.

Source preparation, build and private-state validation share a 30-second
pre-publication deadline and observe claim cancellation. Candidate Context loading is paged and cancellable; valid long histories have
no aggregate timeout. Observers receive transient rebuild status at entry and exit,
including cancellation, without per-page publication. A changed builder cannot reuse another builder's opaque checkpoint. Publication itself is never abandoned on a timer.
Within a Turn, a rejected selection at the same durable Domain cut is not
rebuilt again. Context rejection suppresses that selection for the rest of the
Turn, since advancing Facts and Domain state must not trigger another complete
failed replay at every Step. A changed selection or new Turn permits another
Context attempt; ordinary validation rejection also retries after Domain changes.
This transient rejection memo retains identity only, not a candidate pin.
Preparation deadline failures and source preparation failures before candidate
identity is available are visible bounded rejection diagnostics and stop further
preparation for that Turn. The next Turn retries current source, including repairs
made after the failure. This avoids repeating a failed probe at every Step without
a stable candidate key. Ordinary transient capacity pressure remains retryable;
source shutdown remains an execution failure.

A selection is a diagnostic digest of the evaluated Profile and current executable
catalog. It grants no historical executable recovery and is not a CAS reference.
No evaluated executable recipe is persisted. Already admitted requests, Program
runs and retained jobs own their immutable pins until completion. Cold recovery
uses current plugins and validates retained Domain identity, version, payload and
canonical references at the owning boundary. There is no automatic migration,
reset, or fallback to historical code. Missing mandatory history projections fail
explicitly. Framework history and committed outcomes remain authoritative.

A Kernel generation commit records selection metadata, changed Domain snapshots
and explicit removals atomically. Unchanged state retains its revision and canonical
position without another payload/version row. Changed state advances its revision
once. Introduced or reintroduced Domains start at the generation control sequence,
so an old guard cannot become valid again. Ordinary mutations still advance once,
including equal-value writes. Historical cuts and forks use the membership at their
requested cut; removal affects the active set only. Store adapters index canonical
positions and removal cuts, without duplicating payloads. History and its canonical
references remain until explicit Session/subtree purge; there is no automatic
within-Session compaction or generation-count ceiling.

Each canonical Domain snapshot includes its validated sorted bounded durable
reference set. Typed proposal construction derives it from the encoded state once.
Current-plugin admission verifies it against the visitor. Offline maintenance needs
no plugin code: it verifies framing, canonical references, index coverage and CAS
existence. It cannot re-execute an unloaded plugin's business rules.

The six native Agent-control Tools are thin adapters over the Turn service and
receive caller authority only through the generic typed Tool execution extension
slot. Presets are bounded Profile sources for allowlisted Agent-plane
contributions; provider factories remain in the global Profile. Composition
uses ordinary child Fibers in the same Runtime and does not provide a second
runtime, privileged Host catalog, or hidden service locator.

A cold command may publish a current-composition adoption before evaluating its
callback. If that changes the durable control cut, the command's original review
revision conflicts and the client must refresh before submitting again. No callback
or effect runs under a revision that the caller did not review. Read-only discovery
and projection never publish this adoption.
