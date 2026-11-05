---
name: Separate inbox suspension from pending message horizons
---

## Problem

DSH's Agent keeps queue contents separately from its driver's wake request.
Cancellation with `keepInbox` retains messages without starting another driver
merely because the queue is nonempty. RSI's ready projection previously made
every waking pending message immediately claimable, so cancelling a Turn could
start another Turn from the retained queue. Changing a bridge default cannot
represent this distinction.

## Decision

The [Session protocol](../../../../crates/rsi-agent/session-protocol/README.md)
records inbox pause and resume independently of message acceptance, targets,
bindings and consumption. The mechanical Store projects one boolean per Session;
readiness excludes paused Sessions without rewriting their candidate messages.
Mailbox, summary and tree inspection expose this state at their existing durable
cut. SQLite verifies it against the canonical control history on validation.

The [Kernel](../../../../crates/rsi-agent/kernel/README.md) rejects a stale mailbox
claim while paused. A new waking input resumes within its acceptance transaction;
receipt replay, nonwaking injection, replacement and terminal promotion preserve
the pause. Command inputs preserve their contiguous acceptance records before the
resume control. Continuation and detached Program wake producers append resume
without an extra Store read; an already unpaused inbox tolerates this idempotent
control. Completion notices retain their existing wake policy.

The existing Turn-service cancellation target now includes a Session cut.
Submission admission captures every accepted nonterminal Turn and the inbox;
one commit pauses scheduling, optionally discards pending messages, requests
cancellation and settles a discarded Program child's initial input. A later
waking acceptance is outside that cut. The admitted commit survives caller
detachment. Its receipt acknowledges durable admission rather than cleanup.
An unpublished draft clears held inputs in one revision without publication,
while preserving domain state and command receipts.

Exact Message/Turn requests retain their identity semantics. A Session stop is
a new capture, not an automatically replayable request against future work.
After a lost Store acknowledgement, the Kernel proves the expected controls
and Facts in bounded pages. A proved prewrite failure leaves execution usable.
When both acknowledgement and proof are unavailable, affected resident Fact
cursors must stop writing until canonical state is recovered; otherwise they
could reuse an already committed sequence. The current failure policy uses
Kernel recovery. Automatic in-process recovery is not implemented and is not
claimed to be inherently impossible.

The bridge's ordinary Session cancel explicitly retains the queue and receives
native suspension. The DSH Agent contract defaults to clearing input. Its
synchronous clear records at most two horizon splices and uses one native batch
request. An Inbox-only cancellation target shares batch removal with Session
stop while preserving execution and suspension; it does not load a composition.
Every input dispatch captures its accepted application prefix so later waking
input cannot overtake a cancellation staged between them.

An unconfirmed optimistic clear reconciles against native pending state without
repeating the native capture. Restoration events retain the cleared acceptance's
identity, envelope and native ordering. This extra journal state follows from
combining DSH's synchronous projection contract with an independently durable
executor. DSH itself rejects duplicate pending presentation IDs. If a failed
clear leaves an old native input while a new local input reuses that ID, the new
dispatch waits for explicit resolution instead of hiding or replacing the old
acceptance. A new explicit clear resolves both.

DSH's `whenIdle` follows replacement work until the driver converges. Native idle
waiting uses the existing Session change subscription and indexed activity flags
under submission admission. It includes all open Turns, activation cleanup and
unpaused waking input without enumerating messages or loading a subtree. This
replaces the bridge's 25-ms full-inbox disposal loop. Bridge callers share one
pending idle wait and follow newly accepted application writes before resolving.
It is an observation rather than a lease: exclusive between-turn maintenance
still needs its own native admission contract.

Owned publication completion drains transitive writes until its chain stabilizes.
A cumulative 64-write cap did not bound retained data and could reject ordinary
notification work after earlier writes had settled, so it was removed. A sealed
HTTP response still rejects direct staging; a later Agent notification starts
an independent application-owned publication. The original DSH fused dispatcher
supplies the Agent subject and isolates failing notification listeners, with one
scope carrier per Agent instead of one allocation per event.

## Alternatives considered

Clearing pending messages cannot implement `keepInbox`. Reclassifying each
message as nonwaking would lose its original horizon and require per-message
durable writes and restoration rules. A bridge-only flag would disappear on
restart and would not fence native ready workers. A timer or retained idle task
would add scheduling cost without making the state durable.

Deriving pause from a Turn terminal outcome would also conflate an old cancelled
Turn with a newer waking input accepted while that Turn was finishing. Explicit
controls retain their transaction order without another owner or request loop.

## Consequences

The format, schema and Session interface versions intentionally advance, as
specified by their owning contracts. Existing incompatible stores are rejected;
there is no implicit migration or upstream DSH revision-hash gate.

There is one additional indexed Session value and pause/resume history records,
with no new task, timer or IPC round trip for ready reads. SQLite tree inspection
joins the Session row once for pause and watermarks instead of issuing two
correlated watermark subqueries. This is a structural reduction, not a measured
whole-application speedup.

Ready-index queries use a point lookup of the pause bit instead of a join:
SQLite statistics otherwise change the work done before the page limit. The
existing duplicate-message work bounds remain intact. Tree inspection uses a
left join so a missing Session remains observable as corruption instead of
silently removing a descendant. No query-work threshold is relaxed.

Stop preparation reads indexed control metadata instead of full mailbox bodies
or an Agent subtree. Retaining the queue omits both pending-ID enumeration and
the Header read; an idle stop does not need to pin its unavailable composition.
Acknowledgement reconciliation shares one Kernel implementation with Program
commits, replacing one read per record with bounded pages. These are bounded
work reductions, not a claim of whole-application latency improvement.

The checkpoint golden changes only because its two Header fingerprints bind
Session format 30. Reconstructing the previous format's fingerprints and both
envelope digests reproduces the previous golden exactly. Payload format 16,
envelope format 6, builder version and allocation bounds remain the same.

Memory and SQLite contract tests cover retained contents, atomic rollback,
readiness, reopening and projection tampering. Kernel tests cover stale claims,
atomic wake acceptance, nonwaking commands and receipt replay. Whole-agent
cancellation adds commit barriers, detached callers, new wakes, draft receipt
replay, Program child settlement, unavailable compositions and lost-acknowledgement
recovery coverage. Bridge tests exercise the original DSH inbox projection,
single-request bulk clear, prefix ordering, cold reconciliation and late native
removal after restoration. Native idle tests cover replacement work, paused
queues, shutdown and the absence of timed metadata reads. Between-turn maintenance
remains follow-up work, not an unavoidable tradeoff.
