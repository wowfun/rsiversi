# rsi-agent architecture

Agent is a composition of ordinary process-local plugins. No package-wide
adapter owns their plugin identity. The SQLite Store, Kernel, and executor each
export their own factory; the product composition root assigns stable plugin
and instance identities and places them in a Profile.

The durable boundary is deliberately narrower than the runtime boundary.
`rsi-agent-session-protocol` owns validated session identities, the immutable
session header, and append-only Facts. `rsi-agent-store-protocol` owns only the
mechanical persistence seam and its compare-and-append rules.
`rsi-agent-turn-protocol` owns the process-local submit, cancel, observation,
and outcome service. Runtime scheduling and recovery policy belong to the
Kernel. Model prompt projection and compaction belong to
`rsi-agent-context`, not to the executor or Store.

```text
SQLite Store --Local--> Kernel --Local Turn service--> callers
                            ^
                            |
                    executor registration and claims
                            |
            Agent composition pin -> immutable Tool catalog
                            |
          Preset Profile contributions over global providers
```

The Store root has one cross-process exclusive writer lease held from open
through shutdown. SQLite uses one exact schema version and never silently
migrates or accepts an older layout. Store commits are atomic append operations
against an expected durable sequence. The Store does not allocate identities,
interpret effect transitions, choose recovery outcomes, or schedule turns. It
does transactionally index exact turn membership and the presence of a
terminal Fact, so cold outcome reads and recovery do not scan unrelated
history. Its CAS accepts immutable bounded bytes by digest and never treats a
caller-provided path as owned data.
Store open validates ownership and the exact schema without scanning dormant
history. First access lazily validates one session's mechanical watermark,
stored digest shape, and Fact/turn indexes; only the explicit offline verifier
decodes every Fact and recomputes every canonical prefix digest.

The Kernel owns an in-memory speculative suffix after the Store's durable
prefix. It publishes nonterminal Facts immediately to live observers. Its
per-Session submission admission serializes every speculative suffix mutation
with direct Agent-control commits and remains held until a successful direct
commit is reflected into resident state. A durable Store prefix therefore
cannot advance past a concurrently retained speculative suffix. The single
write-behind worker scans for eligible ordered batches when notified or
after a 200 ms idle interval; Store admission and I/O determine when a selected
batch commits, while an explicit flush is the durability barrier.
After each actual scan, the next periodic deadline is rebased to at least 200 ms
after the current clock so a Store cycle slower than the interval cannot turn
missed ticks into an unbounded catch-up spin.
Every durable commit is a contiguous prefix of the already-published live
stream, except that the sole terminal Fact enters observation only with the
commit that makes its complete prefix durable. A flush failure puts the affected
session into a paused state, keeps the exact suffix queued, and prevents the
executor from starting another external effect until that suffix commits. A
latched permanent failure also rejects later submissions to that session with
the same flush error instead of admitting unreachable work. The Kernel retries
with bounded backoff; it never drops, reorders, or reports the failed suffix as
durable.
Terminal completion performs a bounded final flush.

Ordinary draft creation remains process-local. Its immutable Header becomes
durable with the first accepted Turn or mailbox message. A spawned child is
therefore allowed to have a durable Header and control records while its Fact
tail is still zero. Session listing, attachment, validation, and observation
must treat the Header and the Fact/control watermarks as independent durable
dimensions; outcome reads still require an exact Turn identity. Once durable,
the Header's preset identity, canonical workspace path, frozen settings,
default model, and creation-time permission facts never follow later
configuration drift.

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
Fiber to become Active, seals the exact catalog, and only then publishes the
generation. Candidate failure disposes the complete stage and never replaces a
healthy current generation. Construction is single-flight per preset identity
and source digest. A superseded generation remains alive while a draft,
resident session, or admitted Tool result holds its pin, then tears down after
the final pin releases.

The preset catalog and generation builder share one application-supplied frozen
Profile compiler. Fresh roster discovery compiles each winning source, including
required includes and pure expressions, checks enabled contribution identities
against the frozen Agent-only allowlist, and keeps failed rows visible with a
bounded categorical diagnostic. The roster receives neither concrete factories
nor the Host catalog. That health is observational only: generation selection
probes the exact preset id in root-precedence order without compiling unrelated
roster rows, compiles that selected source once, and then resolves it against
the Agent-only factory allowlist before any Runtime mutation. The catalog
neither receives nor exposes the Host factory catalog.

Startup recovery enumerates sessions with open turns through its bounded Store
index. Waking roots remain in a separate durable ready index and are enumerated
lazily when an executor claim asks for work. Recovery reads only open per-turn
Fact streams into compact live control state and does not materialize all
mailboxes or dormant tree history. A ready message is claimed only when an
executor lane requests work, subject to resident-session and per-tree running
bounds. Recovery repairs every accepted nonterminal turn. A
durable cancellation becomes `Cancelled`; every other unfinished turn becomes
deterministically `Interrupted`. Terminal turn controls and idle sessions are
not retained: historical headers, observations, and outcomes use indexed Store
reads on demand, while the Kernel keeps only a bounded set of sessions with live
or speculative work. Concurrent resumes of one idle session join one in-flight
control-state load. This prevents valid lifetime history from becoming resident
scheduler state or a repeated claim scan.
No accepted Turn is replayed automatically after process loss, including work
with no durable effect-start Fact. Mailbox messages are a distinct durable
queue contract: an unclaimed waking message remains discoverable through the
bounded ready index and creates a new Turn exactly once at claim. External
model, Image, and Tool effects are never repeated by recovery.
`NextTurn` is exactly the waking delivery horizon and `NextStep` is exactly the
non-waking horizon; no other target/wake tuple is valid durable input.

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
A missing or broken cold preset therefore fails before workspace mutation,
resident capacity, Fact materialization, or external effects. A resident
session continues using its existing pin across source changes; after idle
eviction or process restart, preparation deliberately acquires the latest
generation for the same durable preset identity. Dropping an unsubmitted token
releases its pin and has no Store or workspace semantics.

Attaching a durable Session is deliberately narrower than preparing execution:
the application reads its validated Header and exposes history, cancellation,
observation, and approvals without consulting current presets, provider routes,
filesystem state, or the Workspace registry. A later submit prepares only the
dependencies of that operation.

One executor generation may run a bounded number of claim lanes. The Kernel's
durable ready indexes and per-Session claim gate remain authoritative: separate
Sessions can make progress concurrently, but a Session never has two active
turns and its next turn remains blocked until the prior terminal Fact is
durable. Within an Agent tree, ready messages retain their durable timestamp,
Session, and control-sequence order. A bounded root scan skips trees already at
the three-running-Turn cap, so the standard four-lane product retains progress
for an independent Session. This admission is process-local; the standard
Session Host's exclusive owner keeps the scheduler singular, and the Store does
not advertise a distributed multi-Kernel lane lease. Parked activations hold no
executor lane and count only against the 256-node durable tree bound. One activation coordinator owns
lane shutdown and shared retained-effect cleanup; there is no second scheduler
in the executor. Waiting for Kernel work holds no execution lane. One next
claim may wait for bounded admission, and shutdown releases that claim. A
settled lane closes its parking authority and returns admission even if a
retained Tool still owns a copy of the typed execution extensions.
An activation terminal removes its resident Turn, immediately makes the next
oldest accepted Turn claimable, and releases an otherwise idle resident
Session. Recovery first resumes a durably parked wait as cancelled before it
transitions the interrupted activation to descendant settlement.

The executor follows one ordering rule for every external effect. The Kernel
rejects a start marker unless its matching intent is already durable, so an
executor cannot collapse the first durability fence into one publication:

```text
prepare immutable input -> publish intent -> flush durable intent
-> publish start -> flush durable start -> invoke -> publish outcome
```

Language calls and Image calls are pinned process-local objects obtained from
exact active Local generations. Tool definitions and dispatch come from the
same immutable Agent composition pin for the complete claim. Tool execution
uses retained identities:
recovery may query the exact owner/call/request identity, but absence never
authorizes implicit replay. Durable Tool results contain only bounded text,
canonical JSON, and immutable Media references; Agent does not copy media
bytes into Facts.

Direct Image turns durably accept the provider-neutral request, then flush
Image intent and start before provider I/O. Each closed provider output is
imported through Media and its ref is flushed as an ordered Fact before the
next output is accepted. A tail failure records `partial_failed` with every
already-durable ref; retries are separate turns and never overwrite refs.

The Kernel also owns an ordered effect-owned pre-terminal finalizer registry.
The executor runs its snapshot before the sole terminal Fact and applies its
validated finalization deadline to the complete call. Deadline expiry becomes
the turn's durable finalization failure; it releases the executor waiter but
does not claim that arbitrary third-party work was forcibly stopped. The
standard Session composition installs a Jobs finalizer whose own contract
cancels and boundedly joins unfinished process-local work.

The Turn service is the single application seam. Product input first returns a
durable mailbox receipt with its exact acceptance control sequence and indexed
state; it does not invent a Turn identity before scheduling. Claim atomically
creates the Turn and Step, and reconnectable Session observation exposes that
identity and all later Facts through independent control and Fact cursors.
Submission resolves the session default and invocation override into one exact
durable execution policy. Unconfined execution always requires a live approval
decision. The executor pins the Approval and Sandbox generations, records the
approval evidence before Tool start, and passes the exact policy plus Sandbox
authority into the Tool execution boundary. A Tool result retains the truthful
enforcement stamps produced by process plans; a requested mode is never itself
treated as enforcement.
Cancel is idempotent and terminal outcome is single-assignment; a durable
cancellation fires its live token even when the requesting future detaches,
and wins classification even if a provider concurrently returns another
terminal event. An executor cannot classify a turn as cancelled without that
durable request; the Kernel converts such a proposal into a bounded failure.
Observation carries monotonically increasing durable control records and live
Facts plus durable watermarks so callers can distinguish live output from
cold-recoverable state. `outcome` and observation both withhold the terminal
until its prefix is durable. A latched permanent Store failure terminates every
attached observation with the same flush error instead of leaving an
application waiter blocked behind an unreachable terminal Fact.

`rsi-agent-context` incrementally folds Facts into provider-neutral messages.
One claim reads a filtered horizon: all earlier turns and the claimed turn are
visible, while later accepted turns remain invisible even when the claimed
turn's own later executor Facts have higher session sequences. Store pages are
bounded by both Fact count and aggregate encoded bytes. Context folding
compacts complete oldest turns while pages arrive, inserts one deterministic
omission notice, and never retains complete lifetime history merely to project
its bounded tail. It never splits a tool call from its result. Workspace
contents are not implicit input: a model sees them only through an explicit
context source or Tool contract.

A claim carries the exact sequence of its own acceptance. The executor may
restore a session checkpoint only when that checkpoint ends before this
sequence; checkpoints that already folded the claimed or any later accepted
turn fall back to the canonical claim-filtered replay. Checkpoint maintenance
may still encode the complete durable tail for reuse by later claims. Its
optional writer keeps the latest request per Session and preserves FIFO across
distinct Sessions, so a hot Session cannot overwrite or starve another
Session's cache request.

One next-Turn mailbox claim starts an activation and one Step. Safe-boundary
messages close the current Step, start the next, and become Facts atomically
with their durable claims. Turn terminal and activation settlement are distinct:
the terminal closes model execution, while an activation with non-quiescent
descendants remains durably waiting. Settlement is admitted only by a Store
transaction that still observes every descendant without an active activation,
open Turn, or waking message. Child settlement atomically consumes its
per-activation parent-mailbox reservation and emits one completion message;
the message wakes an idle parent and becomes next-Step input for a running one.
An unsuccessful activation terminal concurrently attempts durable cancellation
of every currently open descendant Turn under one cumulative durability
deadline, aggregates failures only after addressing the complete bounded tree,
then releases its lane while settlement waits for those descendants to close;
accepted mailbox messages are not erased.
The `interrupt_agent` Tool remains narrower and cancels only its exact target's
current Turn without cascading.

Agent message horizons are selected by the operation, not by a racy read of the
target. `send_message` always targets the next Step and remains held while idle;
`followup_task` always queues a waking next Turn, including behind a running
Turn. Completion messages are the only Kernel-selected case: they enter the
parent's next Step while its activation is running or parked and otherwise
wake a new Turn. Direct Turns have no Step mailbox and leave fixed-horizon
next-Step input held for the next activation.

A fork creates a new durable, continuable child identity from a tamper-evident
balanced prefix ending before the invoking Turn. The child preserves its
parent's frozen route and policy, while its next provider request retains the
complete visible canonical prefix without forwarding response-level replay
state or provider-private reasoning. A replay extension's namespace and version
do not prove endpoint, configuration generation, or credential identity, and
those exact facts are currently exposed only after request construction. Replay
therefore cannot authorize prefix elision or cross-session forwarding until the
AI seam gains an exact provider-I/O-free route preflight.
`none`, `all`, and a positive completed-turn count are explicit selections;
the current unbalanced Turn is never inherited. The Store indexes terminal
prefix digests. A cold replay revalidates the immutable boundary once at its
first cursor, then pages the sealed interval without repeating a full-prefix
selection query for every page.

The six native Agent-control Tools are thin adapters over the Turn service and
receive caller authority only through the generic typed Tool execution extension
slot. Presets are bounded Profile sources for allowlisted Agent-plane
contributions; provider factories remain in the global Profile. Composition
uses ordinary child Fibers in the same Runtime and does not provide a second
runtime, privileged Host catalog, or hidden service locator.

Tool overlap is opt-in at the Tool-definition owner. The executor may overlap
only one contiguous source-order run whose every definition is `parallel_safe`;
an exclusive Tool is a barrier. Each Tool intent records that scheduling proof,
the Kernel rejects mixed or undeclared active effects, and result Facts are
published in original call order regardless of settlement order. If one member
fails before producing a result, every successful sibling is still published in
source order before the first failure is propagated. An `exclusive_final` Tool
also requires the last source-order position in its model response;
`wait_agent` declares this scheduling class.

Workspace instructions and skills enter a Step only through the process-local
Workspace Context service; the Kernel remains the sole writer of their durable
`InputMessageEntered` Facts. A Session Header freezes `untrusted` or `trusted`
workspace trust at creation and a fork preserves it. User-owned instruction and
skill roots are always eligible, but an untrusted workspace contributes neither
project `AGENTS.md` files nor project skills. A trusted workspace discovers the
nearest `.git` ancestor, reads `AGENTS.md` from that root down to the Session cwd,
and scans only the root-level `.agents/skills` directory. Reads, files, entries,
individual sources, rendered messages, and total batches are bounded. Complete
instruction and skill-catalog digests suppress unchanged refreshes; a later
empty snapshot durably tombstones or replaces an earlier nonempty view. Skill
names resolve in trust order: a project skill never shadows an identically named
user skill. Only direct Human messages may invoke a skill, and invocation loads
the exact already-selected skill body as the final context input for that Step.
The Kernel refreshes the complete snapshot before every provider request, so a
successful Tool round cannot hide an instruction or catalog change even when a
general shell command cannot enumerate its filesystem touches precisely.
