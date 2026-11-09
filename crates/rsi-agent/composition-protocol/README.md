# rsi-agent-composition-protocol

Native Agent plugins register `ToolAround` contributions in the same owned,
immutable catalog as their other callbacks. They await a consuming
`ToolContinuation`: borrowed call arguments and execution authority are readable,
but the admitted body cannot be invoked twice or replaced with different arguments.
Callbacks enter in frozen contribution order and unwind in reverse order. A
callback may return without invoking the body. The entire chain runs inside the
Tool owner's cancellation, timeout, panic and retained-result scope, after policy,
human approval and durable start. It adds no process calls or callback tasks.
The captured chain retains the generation owner until it settles, even if the
requesting waiter drops its composition pin.
An empty chain does not allocate a wrapper.

The native chain wraps the request application's existing execution continuation,
if present; application-provided Tool bodies retain their own JS pipeline. This
is an explicit native/application boundary, not a merged Cordis listener order.
Porting a JS listener with mixed ordering dependencies requires preserving that
position separately. Withdrawal cannot alter an admitted composition pin; a new
generation observes removal or replacement. Recovery observes the final retained
result and never reruns either native hooks or the application continuation.

A request Tool capture can supply around middleware for native preset Tools.
The capture's own added Tools already execute in the application. A native
call keeps its existing preparation, registration, approval and retained-result
identity. The application may consume its already-admitted body once or return
an outcome directly. Recovery reads the final retained outcome and never reruns
the hook or the body.

A captured application request may retain an input preparer beside its Tool
runtime and system guidance. The preparer receives the final borrowed claim Facts
after ordinary contributions, the native Turn/Step and captured Fact horizon. It
returns one typed ordered `StepInputDecision`; Kernel resolves its coordinates
before publication. Capture and preparation share the BeforeStep deadline, and
provider retries reuse the published decision. One native Step can contain several
request series; a retained application capture belongs to only one such series.
The same owner remains alive even when its Tool selection is empty.

An unpublished draft can clear all held command inputs in one revision without
enumerating removal requests. It retains domain state and command retry receipts,
so replaying an old command cannot reinsert the cleared inputs. Clearing an empty
draft is a no-op and does not publish a Session.

A configured source can own request input preparation for a Session. Its readiness
is cancellable and precedes the BeforeStep deadline. When it participates, an
unavailable capture cannot authorize sending the original inputs to a provider.
A tools-only source may fall back to the unassigned preset tools.

A `RequestToolSource` captures a supplemental sealed catalog for a single
provider request series. The generation pin continues to own preset tools.
Capture receives the resolved model and reasoning effort for that series, after
the executing claim's selection is resolved and before preparation callbacks.
The runtime is the only retained owner of captured tool schemas. Its registration
view contains names and identities, without cloning the definitions into another
catalog. The same selection and catalog survive provider retries; a later request series
resolves both again. A source must not infer that selection from the Header or a
fresh Session read, which may describe a different choice.
The captured catalog also carries the application's rendered current system
guidance. It is retained with that request's registration view and published as
`rsi.application.system-prompt` Step instructions. An empty string contributes
nothing; the ordinary aggregate Step instruction bound applies. A later capture
replaces the guidance, including clearing it. This is request state, not a
historical user message or a new preset generation.
The catalog can name a stable source of durable receipts. The captured
registration and native owner continue to identify the original occurrence
after its process generation has retired.
The Host can assign named preset tools to its request source. Only that source
can offer those names, including when it withdraws a tool or cannot capture.
Other preset names take precedence. A claim's private structured-output tool
remains reserved. The combined catalog retains the ordinary
tool count and byte limits. Application registrations are exclusive and have
no Program role. Delegated Sessions retain their frozen name restrictions.

Every Domain definition declares an immutable reference policy alongside its
semantic validator: either explicitly no Session/Agent-CAS references, or a pure
bounded visitor over the decoded state type. The exact Domain identity/version
owns this policy. `None` unconditionally promises that the version contains no
references, so extraction does not decode or semantically validate the state.
Typed state admission still runs its validator; `Visit` decodes and validates
before visiting durable state. Changing reference semantics requires a version change.
Visitors inspect typed fields, never arbitrary JSON strings or schema examples.
The erased codec retains no execution/composition authority and can be retained
by Store reference validation after the originating generation is released.
An unavailable exact codec cannot be interpreted as an empty reference set.

Context contributions receive the executing claim's non-durable execution lease.
Finite resource readers receive the current caller's lease independently of the
composition pin. The resource adapter rejects a missing remote lease, a mismatched
location, or withdrawn admission before invoking a reader, and retains the admitted
operation until the finite read settles. The pure Sources operation needs no
execution lease; its caller still owns Session metadata admission. Neither Header coordinates nor a restored
composition recreate execution authority. Local-only embeddings may omit a lease.

Fresh preparation may retain an opaque Execution lease bound to the Header's
location. The caller binds it to a frozen submission, never to the shared draft
or composition generation. Binding validates location identity but does not
replace the execution owner's live admission check at use time.

An optional immutable `CompositionManifest` describes the flat, redacted plugin
instances captured when a generation is built. It contains identities, effective
enablement and implementation origin, without configuration or filesystem paths.
The manifest is process-local and retained with the pin; it is never a durable
Session artifact or a promise that a cold resume uses the same executable bytes.

A claim-scoped Tool view filters both definitions and preparation against frozen
delegation names. Query, wait and retirement retain exact invocation identities.
The base generation and other claims' catalogs remain immutable and independent.

Finite resource requests enter the generation driver as validated opaque values.
The driver validates contribution output and measures its complete response once,
returning an immutable proof with its encoded size. Local retention transfers
that proof without revalidation; a wire decoder establishes a new proof after
decoding untrusted bytes.

Resource contributions provide finite list/read operations outside execution.
The driver retains the selected composition pin and supplies the actual Header
on every call. They have no mutation, subscription or generic service lookup
authority. Requests and results are bounded before crossing the callback
boundary, cancellation propagates to the owner, and Projection remains pure.

Tool settlement contributions are synchronous and effect-free. They receive
the exact ToolIntent, its retained result, the immutable Header and bounded
current domain snapshots, and return only typed domain proposals. They receive
no historical reader, mutation or I/O authority. Executor invokes them before
result publication and commits their proposals and that ToolResult atomically.
PostTool contributors continue to consume already durable source-ordered results.

A settlement may retain a `Publication` observer and immutable payload leases.
This is separate from the pure proposal callback: Executor notifies it only after
verifying the canonical ToolResult and optional domain control receipt. A definite
precommit rejection permits rollback. Cancellation, a lost receipt, or dropping
an unfinished notification calls `unconfirmed`; this is never permission to replay
the Tool or to discard a possibly committed mutation. Notifications are bounded
and cannot alter the committed result. The adapter reconciles an unconfirmed
operation against canonical state before accepting another mutation. Tool runtime
retirement is not a publication notification because cleanup also retires results.

Each domain definition freezes a fork policy. `Inherit` overlays validated
historical state and is the default; `ResetToInitial` retains the target
definition's initial state. Inherited payloads are validated even when reset.
Kernel applies this generic policy while constructing the child baseline,
before admission; plugins do not clear state after the first request.

Internal reserve callbacks receive the authenticated continuation input in their
captured command context, alongside the invocation's request identity, and must
compare them with the reservation they propose.
Ordinary and draft commands receive no continuation input.

Command registrations distinguish ordinary application dispatch from internal
continuation dispatch as process-local metadata. Internal commands remain pure
state proposals, are omitted from ordinary discovery, and cannot run through
ordinary draft or durable command endpoints. This metadata grants no scheduling
authority: Kernel separately authenticates continuation leases and reservations.

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

Draft owners can discard a bounded set of held command MessageIds under the same
mutation lock as publication. Missing identities are an idempotent no-op; a real
removal advances the draft revision once. Existing command receipts remain, so
retrying a previously accepted command cannot resurrect withdrawn input. The
domain baseline, including supplemental context seed, is independent of inbox
withdrawal.

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

A proposal may retain one process-local resource owner for its complete snapshot.
Draft baselines keep that owner per domain through cloning and first publication;
replacing a domain replaces its owner only after the whole batch validates. The
owner must cover every unpublished external object referenced by the snapshot.
Retention supplies lifetime, never publication or execution authority.

Fork inheritance can require an explicit transform. Such a definition cannot be
inherited through the ordinary copy path. A frozen `ForkTransform` contribution
receives the source Header, immutable completed-turn cut, historical domain set,
and a bounded source Fact reader. It returns only target-generation, revision-zero
domain proposals. The whole inherited baseline and transformed batch validate
before replacing the candidate; every transform-required inherited domain must
have exactly one proposal. Transformation cannot execute Tools, send messages,
or publish parent state. Retained CAS owners survive with the unpublished child.

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
Deny settles the individual call without invoking its body. The executor's
[refusal contract](../executor/README.md) defines caller continuation and ordered
PostTool delivery of both results and rejections.
Deny takes precedence, and no policy can relax resolved Turn approval or
Sandbox requirements. Policies inspect the exact prepared Tool call and the
same pinned domain snapshot; they do not produce state mutations.

## Command contributions

Each ordinary command receives a read-only publication outcome handle. Its
move-only owner transfers through callback preparation and admission. Dropping
it before Store admission proves rejection; entering a potentially durable write
first marks the outcome unresolved. Only exact canonical evidence resolves that
state. The owner records the complete durable control or applied draft receipt,
baseline and held inputs before notifying observers. A failed notification cannot
erase that evidence. Reconciliation reads this retained outcome and never reruns
the callback; unresolved evidence cannot authorize rollback. Initial continuation
reservations have no such handle because they admit no publication observers.

Durable command context also lends a cancellation-bound Fact reader clamped to
the same Store snapshot's durable Fact horizon. Draft context has no history.
Neither port grants mutation authority or a fresh uncaptured history view.
Retaining a command's reader cannot retain the Kernel or its Store writer lock;
the reader upgrades a weak owner only during an admitted read.

Commands may return bounded typed message inputs, domain replacements, or both. A command
must produce at least one input or replacement; it need not register a domain
just to publish input. Input-only commands retain the same canonical receipt,
revision check and retry identity. An empty domain catalog also admits draft
inputs; first submission publishes their input-only baseline. `Inject`
holds input for the next Step without waking an idle Session; `Steer` joins the
current Turn when one is active, otherwise wakes a new Turn.
All pending inputs from one command invocation enter that waking Turn together
in their original output order, including earlier injected content. The owner assigns
message identities from the exact invocation and ordinal, with explicit command
provenance. A callback may retain original presentation attribution alongside
that provenance; ordered text, image and file blocks retain their types. The
external admission owner verifies referenced objects before returning the proposal.
Attribution and
content are validated before any owner state changes. All messages and domain replacements publish atomically. A draft
retains these inputs locally, without waking or creating a Session; first submit
publishes them as held next-Step inputs with the actual baseline. Changing the
draft preset clears both its baseline and its pending command inputs. Aggregate
pending input is bounded at 64 messages and 256 KiB, including the draft's whole
unpublished batch. Observers receive the exact selected command messages.
The durable mailbox admits at most 64 pending command inputs. Held command content
joins the next waking input in canonical acceptance order, before model dispatch.
Draft inputs require the Agent mailbox submission path. Direct text or image Turns
without an Agent activation reject such a frozen draft before creating Store state.

Session command callbacks are frozen by the same unpublished registrar and
generation as execution contributions. Each has a bounded description, an
explicit draft-safe flag and a unique command name. Callbacks receive the
immutable Header, typed revision and complete bounded domain snapshot, and
return typed domain replacements, inputs and optional retained publication observers.
They have no external-effect or Store capability. The owning Session or Kernel validates and atomically commits the
complete output after the callback finishes outside framework locks. Duplicate
request lookup precedes callback invocation and revision checks. A conflict
never causes an automatic callback retry.
Publication notification runs after owner admission is released. Durable commands
provide their exact canonical domain control; drafts provide the actual successor
receipt, complete initial-state baseline and held input batch. The draft state
digest binds that complete first-publication candidate. Definite prepublication refusal sends
rejection. A dropped or failed notification retains uncertainty and never licenses
callback replay. Draft application returns a move-only completion whose `finish`
must run outside the lease owner's mutation lock.

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

A bounded `AgentGenerationSeed` carries structurally validated opaque Domain
snapshots into composition before private registrars and plugin activation. Fresh
builds use the source snapshot's current seed; restoring builds receive the stored
Session baseline explicitly. The seed is at most 64 sorted domains and 1 MiB in
aggregate, with the existing 256 KiB per-state limit. Construction computes one
SHA-256 over its canonical JSON, including exact number text and object order.
Clones and equality reuse that immutable identity without serializing payloads. `AgentGenerationInputsContract`
is isolated within the hidden generation. Each consuming plugin decodes its own
state using its Domain codec before declaring Tools or callbacks. The Kernel
neither interprets provider payloads nor mutates an already sealed catalog.

Plugins select saved states through `AgentGenerationInputs::seed_state` when
they require one exact codec. A mismatch records only the actual and expected
typed Domain identities in that private build. Composition reports this closed
diagnostic after rollback, including the action to start a new conversation;
it never exposes arbitrary plugin activation messages or saved state contents.

Tool settlement callbacks return validated domain proposals and at most one typed
conclusion. The Kernel validates and atomically publishes conclusion metadata
with the exact ToolResult. Conclusions forbid further business Facts in that
Turn; the Executor stops before any later unstarted Tool or model request.
Claim-scoped `report_result` is an exclusive, retained pure Tool whose real input
schema is the frozen initial output contract. Invalid values return an ordinary
error ToolResult and consume the existing Turn budget. Retention belongs to the
resident Turn and survives executor replacement; it is not a shared catalog edit.

A claim pin exposes `output_contract()` only when its private reporting Tool was
installed for that Turn. The Kernel selects this from the initial message binding;
Executor settlement uses the selected pin rather than a Session-wide Header hint.
Delegation restricts Tool discovery and preparation. Retained result query, wait
and commit still use the provider's exact identity protocol for recovery of already
admitted effects; this wrapper is not an isolation boundary between trusted Rust
holders. Models cannot call those operations or supply retained-result identities.

The process-free reporting Tool requires explicit execution preparation before
start, so its review participates in the same approval binding as other Tools.

A pin caches the exact encoded weight of its immutable, selected Tool catalog
without cloning definitions. Before a deep copy, measurement enforces the AI
request Tool count, aggregate schema/grammar bound and total request encoding
bound. Clones share that measurement. Delegation and private
reporting views create a fresh cache because they change catalog membership;
Executor reserves options credit from this weight before copying definitions.
## Staged BeforeStep contributions

Fork drafts retain their already transformed baseline and source retention until
first publication or release. Selecting another preset validates every captured
domain against that exact catalog and preserves its value and resource owners;
missing or incompatible codecs reject selection atomically. Newly declared domains
use their defaults. Preset selection never reruns the fork callback.

`BeforeStepContributor` receives one captured context, a framework-allocated
request identity, the selected model and effort, the frozen Tool names and the pending messages from settled
tools. It can inspect these messages while preparing its additions. Executor
publishes the captured messages before those additions in the same Step commit.
The contributor returns ordinary durable context
inputs, typed domain proposals, retained publication observers and an optional
Tool subset. One durable Step may contain multiple provider retry series.
Any contributor can reject entry for the entire request series. The framework records one rejection with the stage outputs and terminates the Turn as blocked before application input preparation or provider dispatch. Multiple subsets intersect; unknown names and duplicates fail the
whole stage before publication. Retries reuse the admitted set.

Executor commits the complete stage, including `StepPrepared`, atomically
with domain proposals before acknowledging staged process state. Observers share
the same exact publication contract as Tool settlement: canonical Facts and
domain receipt, definite rejection, or uncertainty without callback replay.
Such observers require a domain proposal so the request has a durable receipt.
Provider requests and model Tool dispatch use the same selected set; a later
Step cannot grant a detached Program additional Tool authority.

BeforeStep callbacks receive the previous ordered decision for their captured input cut. Ordinary context contributions join that cut after the preset callbacks. The request preparer sees both the preset decision and these new claims before the executor publishes the final input, instructions and domain proposals together. Rejection is terminal for input editing.
An application may prepare a native invocation on the captured Agent after
native policy and process planning. A request for approval contributes its
reason to the existing exact Tool review. A refusal precedes the durable intent.
Successful preparations belong to their individual calls until start or drop.

Command preparation consumes callback-owned input blocks. It measures each held publication once using its compact wire encoding. Draft capacity checks sum those immutable lengths, including JSON array punctuation; a durable owner can move the prepared message directly into its publication.
