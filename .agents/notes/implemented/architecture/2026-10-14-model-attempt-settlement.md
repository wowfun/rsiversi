---
name: Model attempt settlement without cumulative Turn allowances
---

## Problem

Persisting each provider fragment charges a transport-dependent number of Facts
and prematurely exhausts cumulative Turn allowances. The retained Windows Web
evaluation reached 65,537 generated records, although the model and tools could
continue. Raising that allowance leaves fragmentation-dependent behavior and
does not address record volume or partial-output recovery.

## Decision

RSIversi removes cumulative Turn allowances and output recovery. Independent
resource capacities, domain round limits, cooperative cancellation and external
evaluation deadlines remain. Each model attempt is persisted atomically after
completion or controlled interruption, retaining lossless bounded evidence. Kernel owns live
stream admission and observation; Store remains mechanical persistence.

Remote live observation sends an initial prefix followed by offset-checked byte
suffixes. Incremental grammar validation retains charged prefix ownership between
revisions. Source-free terminal scene text crosses the same bounded display
boundary and invalidates cached layouts on change. Hosts may raise logical model
reservations in whole attempt units; this remains a pre-dispatch reservation,
not measurement of resident memory. Automation reconciles a fixed durable horizon
at expiry and judges terminal timestamps against the accepted round's limit.

Raw-pressure compaction must make material progress before another summary call;
this bounds ineffective regeneration without restoring a cumulative Turn
allowance. CLI presentation keeps one charged early model snapshot until its
start Fact arrives, then shares the settlement cursor to avoid replaying text.

The Session, Kernel, executor, Context, AI, DeepSeek and Automation package
contracts own their exact capacities, defaults, wire forms and ordering.
Safe interrupted text enters later model context; incomplete tools never gain
execution authority. DeepSeek language regeneration may retry an uncertain
transient outcome while preserving truthful dispatch evidence.

This reverses the automatic continuation decision in
[model output recovery](../../implemented/bug-fix/2026-10-10-model-output-recovery.md).
The relevant typed-commit, execution-contribution, Goal, context-compaction,
tool-origin, request-evidence, live-UI and automation decisions retain their
independent authority and lifecycle invariants.

## Alternatives considered

Increasing or disabling the default Turn allowance retains a policy the user has
explicitly removed. Persisting every fragment without allowances retains write
amplification. Coarsening retained timing silently loses evidence. Multiple
durable chunk Facts require an additional seal/reassembly protocol without a
demonstrated need beyond the bounded single-Fact representation.

A fixed small summary-call cap can reject useful bounded batches before they
reach the hysteresis target. Requiring proportional reduction in a pressured
dimension instead rejects negligible progress while admitting large histories.
Decoded full-log caches would introduce another long-lived evidence owner and
retention budget. Contiguous payload copying improves decoding without adding
that ownership or relaxing external canonical validation.

Settlement acknowledgement instead carries an issuer-bound publication receipt
and checks the resident durable watermark, avoiding a result-body reread.
Independent paging retention follows the
[Session-owned evidence decision](2026-10-18-evidence-section-retention.md).

## Consequences

There is no cumulative Turn provider-call ceiling or automatic Turn wall-clock
watchdog. A long Turn can make arbitrarily many billed requests; proportional
compaction progress can still require many summary calls. Provider-specific
timeouts and cooperative cancellation remain, but a provider that never returns
from preparation needs cancellation from an external owner. Evaluation deadlines
bound evaluations, not every interactive Turn.

Language regeneration permits both known-dispatched and unknown-dispatch
transient failures; an uncertain outcome does not imply uncertain dispatch.
The configured five retries can pay for six generations for one logical request.
Session, Store and checkpoint version changes reject older persisted data; this
pre-release change provides no migration and requires a fresh compatible store.

A process crash loses the current uncommitted attempt; recovery reports that
interruption without inventing output. Exact per-attempt evidence and live
memory limits can stop a large generation with a retained prefix. Regeneration
after uncertain transport failure can incur duplicate model charges. Reusing
the previously observed Web task provides non-blind regression evidence only.
Native macOS behavior remains unverified by this implementation task.

Deterministic tests cover former cumulative thresholds, active-Turn compaction,
controlled settlement, Memory and SQLite replay, language regeneration, and idle
SSE behavior. Native Windows evaluations retain the comparison executables and
source manifests separately from later consumer fixes. The WebGL run retains
212,535 content deltas in 96 settled attempts without a record-budget failure;
it still fails the independent unreported-background-work rule. Artifact grades
do not override that terminal result. Browser checks cover live and settled
Unicode output across refresh, a single source reference and visible output-limit
termination without automatic continuation.
