---
name: Program execution authority and durable Tool provenance
---

## Problem

A model-only ToolIntent format consumes every call from an exact completed Conversation
response in `kernel/src/tool_origin.rs`. Context locates that same call in
`context/src/outcomes.rs`. A program cannot reuse a model call identity for its
internal calls, and a detached workflow cannot retain its creator's retired Tool
claim. Fork lineage alone also routes child completions into the ordinary parent
mailbox, whose 64-message bound is independent of workflow concurrency.

## Decision

The protocol uses explicit Model and Program Tool origins. A foreground program call records
its exact started coordinator effect and monotonically admitted call ordinal.
The sealed Local Tool definition declares Unavailable, Callable, Coordinator or Workflow;
Portable definitions remain Unavailable, without a Portable wire change. Kernel
validates the durable declaration against that pin before publication, enforces
parent lifetime and overlap, and charges ordinary Tool and byte budgets. Recovery
validates historical provenance structurally and interrupts unfinished work; it
does not compare historical declarations with a different cold generation. Such
a comparison requires the original catalog, which is not durably retained. Context
retains provenance state for replay/checkpoints but forwards only the outer
program result. Program calls cannot recursively call coordinators or workflow creators.
The separate Workflow role authorizes detached creation from the exact started
model-origin effect; a product Tool name cannot grant that authority.

ProgramRun ownership is separate from immutable fork lineage. A detached run owns
its generation, frozen parent horizon, model and policy, initial child admission
allowance, durable progress and exclusive child-completion sink. Its authority
is minted before creator retirement; old TurnClaims are never revived. The Kernel publishes
one bounded completion notice after the run ends. Restart interrupts runs and
revokes unclaimed owned children/notices without replaying code or provider I/O.
Independent human input in those branches survives recovery. It can keep an old
initial activation waiting after its Turn has been interrupted, so startup retires
the run without waiting for that receipt. A late initial settlement is consumed
by the interrupted sink without changing its terminal ledger or publishing an
ordinary completion. Live run settlement still joins receipts; later human
activations retain their ordinary completion routing.

A common native Node runtime serves foreground `run_code` and workflow scripts
through Process duplex framing and Jobs. Process owns spawn/reap and Jobs owns
live controls; Agent owns durable provenance and run policy. Each workflow scope
includes its immutable run identity, and cleanup precedes terminal publication.
Otherwise a predecessor can cancel a successor or report cleanup failure after
already publishing success. Owned finalization retains child draining beyond the
interactive cancellation deadline instead of abandoning an unfinished ledger. Scripts have shell-
equivalent authority under the exact Sandbox plan, with no Node VM security claim.
The JavaScript client-foundation decision remains applicable to application
business logic; these scripts are explicit user-authorized execution effects.

SQLite indexes activation counterpart coordinates from canonical control JSON.
This adds write-time expression-index maintenance but keeps graph proofs inside
atomic commits independent of unrelated control history. The exact schema is
advanced; pre-release stores are rebuilt rather than migrated. The lineage index
retains a bounded execution-owner projection verified against the Header. This
avoids decoding up to 255 full descendant Headers inside warm subtree reads and
write-time idle guards while preserving cold canonical validation.

Foreground coordinator completion settles an already-started nested call under
its original cancellation/timeout instead of cancelling the shared Turn token.
A cancelled child waiter releases after bounded source-mutation draining; actual
child terminal settlement remains owned by the Program run. Tool Runtime exposes
exact-name definition lookup plus role-only projections so claim setup and role
checks do not copy schema payloads. The Session protocol owns the outstanding-call
bound used by dispatch and durable Context replay; Node receives it at startup.

Live readers share immutable folded state until a new canonical suffix arrives.
The cache still verifies each incremental mechanical head and never publishes a
precommit transition. Cancellation reuses tree snapshots but rechecks membership
after source mutation drain; a one-time membership snapshot could miss a child
admitted concurrently with cancellation.

## Alternatives considered

Inventing model responses would corrupt evidence and context. Retaining old Tool
claims would make detachment depend on a dead authority. Routing every child
completion through the parent defeats bounded curated output and exhausts mailbox
capacity. Durable background scheduling in Jobs would duplicate Agent recovery
ownership. A JavaScript VM alone cannot provide an OS security boundary.

## Consequences

The Session, Store and Context checkpoint formats change together and reject old
schemas explicitly. Node availability is an opt-in runtime dependency. Default duplex tests use fake
byte ports; Linux CI explicitly supplies the pinned Node runtime for integration
coverage. Dropping the last live run owner cancels its token but cannot reconstruct
its lost Jobs handle from history. Such an orphan remains stale until startup
recovery; pretending that a new owner can prove old process cleanup would weaken
the settlement guarantee. Framing is
length-prefixed JSON bounded to 1 MiB and 16 outstanding requests; complete script,
result and run-record budgets remain separate from transport framing.

Structured child and workflow result pages share the Tool protocol's bounded
fragment presenter; identity/revision authentication stays with each consumer.
This keeps UTF-8 and JSON-escaping budget fixes from drifting across two adapters.

Public tests cover source proof, nested policy/approval, bounded presentation,
checkpoint/replay/export, cancellation and retirement. Workflow tests exceed 64
sequential children without parent mailbox growth, detach across creator terminal,
and prove restart interrupts without replay. Live measurements distinguish model
requests, tokens and duration from deterministic mock evidence.

The run record budget reserves 4 MiB of its 8 MiB total for closure. Existing
4 KiB failure diagnostics can expand sixfold in JSON; 128 worst-case child
receipts exceed a 1 MiB reserve. Tests fill ordinary progress capacity before
settling all 128 escaped failures. This limits retained progress sooner in
exchange for a provable terminal path.
