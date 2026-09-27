# Turns and Agent trees

The Kernel owns one elapsed-budget clock per live Turn. Publication admission
and the executor deadline watch read that same clock. A human interaction pauses
only elapsed execution time and releases the exact tree/executor permits;
provider attempts, Tool calls, and generated-Fact budgets remain charged.
The clock resumes after execution admission has been reacquired. Deadline and
park contend on the same state, so parking cannot revive an exhausted budget.
Human waits last until answer or cancellation; ordinary bounded Agent waits
retain their execution-time semantics. Restart interrupts a waiting Turn and
never recreates a suspended human request from its historical Tool intent.

One executor generation may run a bounded number of claim lanes. The Kernel's
durable ready indexes and per-Session claim gate remain authoritative: separate
Sessions can make progress concurrently, but a Session never has two active
turns and its next turn remains blocked until the prior terminal Fact is
durable. Within an Agent tree, ready messages retain their durable timestamp,
Session, and control-sequence order. A bounded root scan skips trees already at
the three-running-Turn cap, so the standard four-lane product retains progress
for an independent Session. This admission is process-local; the standard
Service Host's exclusive owner keeps the scheduler singular, and the Store does
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
Its hooks use Meta's exact-generation Local registration credential and stable
composition positions, following the [finalization contract](../../turn-protocol/README.md).
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

One next-Turn mailbox claim starts an activation and one Step. Safe-boundary
messages close the current Step, start the next, and become Facts atomically
with their durable claims. Turn terminal and activation settlement are distinct:
the terminal closes model execution, while an activation with non-quiescent
descendants remains durably waiting. Settlement is admitted only by a Store
transaction that still observes every activation-owned descendant without an active activation,
open Turn, or waking message. Ordinary child settlement atomically consumes its
per-activation parent-mailbox reservation and emits one completion message;
the message wakes an idle parent and becomes next-Step input for a running one.
An unsuccessful activation terminal concurrently attempts durable cancellation
of every currently open descendant Turn under one cumulative durability
deadline, aggregates failures only after addressing the complete bounded tree,
then releases its lane while settlement waits for those descendants to close;
ordinary accepted mailbox messages are not erased. Program retirement separately
revokes its owned pending automatic work as specified by the ProgramRun contract.
The `interrupt_agent` Tool remains narrower and cancels only its exact target's
current Turn without cascading.

Agent message horizons are selected by the operation, not by a racy read of the
target. `send_message` always targets the next Step and remains held while idle;
`followup_task` always queues a waking next Turn, including behind a running
Turn. Human steering is an immutable ingress intent distinct from the resolved
delivery horizon. Under Session submission admission, a steer binds to the
current non-cancelling activation Turn when available, otherwise it becomes
waking next-Turn input. Same-identity retries compare the immutable intent and
payload before consulting current activity. Unclaimed bound steering is promoted
atomically with every terminal/recovery transition, using its original
acceptance order; explicit message cancellation never resurrects input. Fixed
next-Step Agent messages remain fixed. Completion messages enter the
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

Tool overlap is opt-in at the Tool-definition owner. The executor may overlap
only one contiguous source-order run whose every definition is `parallel_safe`;
an exclusive Tool is a barrier. Each Tool intent records that scheduling proof,
the Kernel rejects mixed or undeclared active effects, and result Facts are
published in original call order regardless of settlement order. If one member
fails before producing a result, every successful sibling is still published in
source order before the first failure is propagated. An `exclusive_final` Tool
also requires the last source-order position in its model response;
`wait_agent` declares this scheduling class.
