# Sessions and recovery

The Store root has one cross-process exclusive writer lease held from open
through the final admitted operation and connection close. SQLite uses one exact schema version and never silently
migrates or accepts an older layout. Store commits are atomic append operations
against an expected durable sequence. The Store does not allocate identities,
interpret effect transitions, choose recovery outcomes, or schedule turns. It
does transactionally index exact turn membership and the presence of a
terminal Fact, so cold outcome reads and recovery do not scan unrelated
history. Its CAS accepts immutable bounded bytes by digest and never treats a
caller-provided path as owned data.
Store open validates ownership and the exact schema without scanning dormant
history. Metadata reads validate bounded Headers only. Explicit validation and
execution/history access check one session's mechanical watermark,
stored digest shape, and Fact/turn indexes; only the explicit offline verifier
decodes every Fact and recomputes every canonical prefix digest.

The Kernel owns an in-memory speculative suffix after the Store's durable
prefix. It publishes nonterminal Facts immediately to live observers. Its
per-Session submission admission serializes every speculative suffix mutation
with direct Agent-control commits and remains held until a successful direct
commit is reflected into resident state. A durable Store prefix therefore
cannot advance past a concurrently retained speculative suffix. The single
write-behind worker independently drives one eligible ordered batch per resident
Session. Wake selection is fair between completion, notification, timer and stop.
A completion replenishes only that Session; notifications and a 200 ms
idle timer prepare all eligible batches under one state lock. Store admission and
I/O determine when a selected batch commits, while an explicit flush is the
durability barrier. Each timer wake rebases its next deadline to at least 200 ms
after the current clock, preventing missed ticks from causing a catch-up spin.
Every durable commit is a contiguous prefix of the already-published live
stream, except that the sole terminal Fact enters observation only with the
commit that makes its complete prefix durable. A flush failure puts the affected
session into a paused state, keeps the exact suffix queued, and prevents the
executor from starting another external effect until that suffix commits. A
latched permanent failure also rejects later submissions to that session with
the same flush error instead of admitting unreachable work. The Kernel retries
with bounded backoff; it never drops, reorders, or reports the failed suffix as
durable.
Terminal completion performs a bounded final flush. Every terminal crosses the
[correlated Fact/control Store commit](../../store-protocol/README.md). The flusher
ends each Session batch at its first terminal. Control mutations fence a queued
terminal under submission admission before sampling their control cursor; the
flusher remains independent of that admission so the fence cannot deadlock.

Ordinary draft creation remains process-local. Its immutable Header becomes
durable with the first accepted Turn or mailbox message. A spawned child is
therefore allowed to have a durable Header and control records while its Fact
tail is still zero. Session listing, attachment, validation, and observation
must treat the Header and the Fact/control watermarks as independent durable
dimensions; outcome reads still require an exact Turn identity. Once durable,
the Header's preset identity, canonical workspace path, frozen settings,
default model, and creation-time permission facts never follow later
configuration drift. A nonempty fresh domain baseline is control one in the same
commit as the Header and first Turn or message acceptance. The draft's actual
payload crosses that boundary; omission denotes the frozen empty domain set.

Startup recovery enumerates sessions with open turns through its bounded Store
index. Waking roots remain in a separate durable ready index and are enumerated
lazily when an executor claim asks for work. Recovery reads only open per-turn
Fact streams into compact live control state and does not materialize all
mailboxes or dormant tree history. A ready message is claimed only when an
executor lane requests work, subject to resident-session and per-tree running
bounds. Recovery repairs every accepted nonterminal turn in a separate correlated commit;
a later startup continues after any already committed repairs. A
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

Attaching a durable Session is deliberately narrower than preparing execution:
the application reads its validated Header and exposes history, cancellation,
observation, and approvals without consulting current presets, provider routes,
filesystem state, or the Workspace registry. A later submit prepares only the
dependencies of that operation.

[Composition generations](composition-generations.md) owns draft pin replacement
and execution preparation. [Turns and Agent trees](turns-and-agent-trees.md)
owns fork creation, immutable lineage and balanced prefix selection.
