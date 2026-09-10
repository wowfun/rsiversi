---
name: Verifiable and steerable CLI coding workflow
---

## Problem

Command outcomes kept only in structured values disappear when the context
forwards nonempty Tool text. Client-memory follow-ups also disappear on detach,
and a human wait must release execution capacity without becoming an
implicitly replayable effect. Scripted provider tests establish plumbing, not
coding capability.

## Decision

Shell and Jobs render outcome evidence at the producer. Process owns a bounded,
completed-only cache and the standard product contributes its read-only paging
Tool. The Kernel durably resolves immutable human delivery intent and promotes
unconsumed bound steering at every terminal or recovery boundary. One Kernel
human-wait guard owns elapsed-budget arbitration and both admission lanes.
Questions use a separate ordinary Base protocol and Host plugin. Approval
reviews one prepared Tool request and execution consumes that same object.

The Session interface exposes atomic Store inspection and separate live
interaction snapshots. The line CLI owns one renderer and independent
observation and interaction-refresh tasks. These tasks cannot suspend a Store
read while synchronously waiting on another read through the same byte budget.
The input reader is a bounded handoff; a receipt always describes durable
Kernel acceptance. Durable formats and the [local connection fence](../../../../crates/rsi/service-host/README.md)
make these contracts explicit without pre-release compatibility shims.

Human-wait cleanup is registered before its handle escapes. A retained lease
can finish its admitted control transition after claim release or shutdown;
dropping the public handle only closes a channel. Shutdown retains the bounded
internal admission pool for these completions and drains them before final flush.
The live answer receipt remains distinct from durable Tool-result publication.

The offline oracle tolerates comments and formatting in the fixed module layout,
checks extra-module rejection, and preserves trace-read failures in reports. Its
wall-clock task deadline and per-Turn execution budgets are reported separately.
The [external coding oracle decision](../testing/2026-09-05-external-coding-oracle.md)
supersedes the in-process completion receipt as scoring evidence. The parent
compares actual function results, while the fixture runs in an isolated PID and
filesystem namespace. Timeout and grading-infrastructure failures remain report
data, so failed grading does not discard the surrounding attempt evidence.

## Alternatives considered

DSH's `packages/shell/tool-bash/src/render.ts` exposes both truncation and exit
markers. Its spill-path interface informed the feedback, but RSI exposes opaque
cache identities to preserve the Process boundary. Pi's
`packages/agent/src/agent.ts` separates steering and follow-up queues; RSI persists
their intent rather than copying its in-memory ownership. Codex's
`codex-rs/core/src/session/turn_input.rs::steer` checks an expected Turn identity;
the selected RSI contract instead falls back atomically to the next Turn.
Cordis's `packages/core/src/fiber.ts::effect` illustrates effect-owned cleanup;
RSI keeps the new capability in ordinary plugin lifecycle ownership.

Durably suspending questions would require durable answer receipts and restart
reconciliation. Completed logs could instead be a Session archive, but that
would require transactional retention and durability barriers. Neither stronger
contract is required by this milestone. Context summarization and richer UI
remain separately scoped work.

## Consequences

The Host bounds pending approval payloads by aggregate encoded bytes as well as
request count. The 16 MiB policy prevents the 4 MiB prepared-review limit from
being multiplied by 1,024 live entries; it is not a measured optimum or an exact
heap bound. Full-size arguments plus metadata can exhaust it with three pending
reviews. Admission failure releases the caller without silently truncating the
prepared effect that a human is asked to approve.

Observation retry exhaustion ends the CLI attachment with an error while leaving
durable Session work owned by the Host. Keeping a client that accepts input after
its observation task has exited would conceal subsequent outcomes and questions.
An attachment switch reads the new snapshot and history before retiring the
current observer. Interactive status reports client detachment, not a single
Turn: one client may observe multiple Sessions, historical outcomes and work
submitted by other clients. Scripts needing a Turn result use the headless
application or inspect the correlated Outcome event. The binary regression
pins successful detachment after a failed Turn without suppressing that event.

Subscription dispatch releases its decoded request permit before the long-lived
stream. Upload frame scratch is separately bounded per connection, avoiding a
second acquire on the pool still owned by the decoded request. The connection
ceiling bounds that scratch independently of aggregate decoded-image admission.

Cache entries may be evicted and are deliberately not fsynced. A filesystem
operation already blocked in the worker cannot be forcibly stopped; its lease
and reservation remain owned, while publication has a bounded timeout. Failed
cleanup closes capture admission rather than recycling unremoved-file quota.
Human waits do not consume execution elapsed budget, but ordinary Tool and
cleanup bounds still apply. Host restart interrupts pending questions and old
Turns; it does not restore suspended waiters or claim durable human receipts.
Recoverable storage failure during resume cannot justify dropping the parked
activation's mutation lease. The caller's bounded wait and owned retry lifetime
therefore differ. Permanent failure handling is partially superseded by the
[retained-wait failure decision](../bug-fix/2026-09-05-permanent-wait-failure-retirement.md),
which pauses the Session before releasing local wait ownership.
The same distinction keeps cache shutdown requested across queue saturation and
provider retirement deadlines. No in-process deadline can settle blocked filesystem
I/O, so a persistent failure continues to consume the affected bounded ownership.

The opt-in coding oracle lives outside the editable workspace. Its self-test
covers failing initial sources, passing reference sources, hostile admission,
and ordinary Cargo formatting and lockfile behavior. Reports preserve failures
and distinguish invalid infrastructure runs from real Agent and budget failures.
An oracle correction regrades retained source offline; it does not license
resampling a valid failed task. Linux integration evidence does not establish
native Windows or macOS behavior, and one passing coding task is not a general
coding-capability claim.
