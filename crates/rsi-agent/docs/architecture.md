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
                    executor registration
                            |
            Language, Image, Media, and Tool Local services
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

The Kernel owns an in-memory speculative suffix after the Store's durable
prefix. It publishes nonterminal Facts immediately to live observers. Its
single write-behind worker scans for eligible ordered batches when notified or
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

Creating an empty session is lazy and process-local. The immutable header and
first `TurnAccepted` Fact are created atomically when its first turn is
submitted. Therefore an empty session that never receives a turn does not
survive process loss; no durable receipt promises otherwise. Once the header
exists, its canonical workspace path, frozen agent settings, default model,
and creation-time permission facts never follow later configuration drift.

Startup recovery enumerates sessions and the Store's mechanically indexed open
turns through bounded cursor pages, reads only those per-turn Fact streams into
compact live control state, and repairs every accepted nonterminal turn. A
durable cancellation becomes `Cancelled`; every other unfinished turn becomes
deterministically `Interrupted`. Terminal turn controls and idle sessions are
not retained: historical headers, observations, and outcomes use indexed Store
reads on demand, while the Kernel keeps only a bounded set of sessions with live
or speculative work. Concurrent resumes of one idle session join one in-flight
control-state load. This prevents valid lifetime history from becoming resident
scheduler state or a repeated claim scan.
No accepted turn is replayed automatically after process loss, including work
with no durable effect-start Fact. Requeueing every known-not-started turn would
make unbounded durable history resident scheduler state; a future durable queue
contract may add explicit retry without weakening the Kernel's bounded-memory
invariant. External model, Image, and Tool effects are therefore never repeated
by startup recovery.

Resume validates the proposed turn body against the durable header before an
idle historical session is admitted to resident Kernel state. Invalid requests
therefore cannot consume the active-session bound. Claim reads return a Store
page before consulting the speculative suffix whenever the durable watermark
advances during Store I/O, preserving one contiguous prefix across races.

The executor follows one ordering rule for every external effect. The Kernel
rejects a start marker unless its matching intent is already durable, so an
executor cannot collapse the first durability fence into one publication:

```text
prepare immutable input -> publish intent -> flush durable intent
-> publish start -> flush durable start -> invoke -> publish outcome
```

Language calls, Image calls, and Tool invocations are pinned process-local objects obtained
from exact active Local generations. Tool execution uses retained identities:
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
does not claim that arbitrary third-party work was forcibly stopped. Standard
Headless installs a Jobs finalizer whose own contract cancels and boundedly
joins unfinished process-local work.

The Turn service is the single application seam. Submit returns a turn
identity, not a durability receipt; callers open live observation separately.
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
Observation carries monotonically increasing live
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

Detach ends observation without changing durable state. Fork is not a v1
operation. Presets are linked Profile fragments that compose the ordinary
plugins; they do not provide a second runtime or hidden service locator.
