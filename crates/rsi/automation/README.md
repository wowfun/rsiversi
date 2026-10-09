# rsi-automation

Automation owns signed GitHub deployment ingress, standing operator rules,
durable admission, attempts, bounded evidence and caller-scoped operations.
It is opt-in in the Linux daemon. A receipt is committed before HTTP 202;
202 does not promise execution. Unknown commits fence this owner, never replay
effects, and do not fence the ordinary Agent Store.

Delivery identity is source plus GitHub delivery ID. Logical task identity is
source, repository ID, deployment ID and stable rule ID, independent of status,
delivery and rule revision. Inputs and execution policy are immutable within an
attempt. Only a complete, durable assertion failure may issue a bounded Goal
under a live standing rule. Pass, infrastructure failure, duplicates, reads and
restart never issue execution authority. Manual resume creates a new attempt
using the current authorized rule and retains previous evidence.

Rules bind a repository, environment, unique anonymous deployment URL space,
entry identity condition, deterministic assertions, runtime/catalog digest and
five-dimensional Agent budget. Page content is external data. Exploration only
navigates and observes bounded text; it cannot click, type, execute scripts,
upload, create children or acquire other Session context. Loading a page can
still cause backend effects, so operators supply expendable preview backends.

The operator keeps the private Automation directories and their ancestors stable
for the activated owner. Startup rejects symlink traversal and unrelated Ledger
entries; child file opens reject final symlinks and nonprivate or multiply linked
files. SQLite/WAL and policy publication use these trusted stable pathnames, not
retained directory-relative capabilities against concurrent host replacement.
Test fixtures resolve trusted temporary-directory aliases before admission;
explicit symlink-rejection inputs keep their original paths.

The dedicated SQLite writer has an exclusive no-follow file lease, immediate
transactions, FULL synchronization and bounded records, database/WAL and PNG
evidence. Events older than the persisted acceptance floor are rejected before
admission. Deduplication records remain until their events are below that floor.
Startup validates every retained attempt but rewrites only interrupted work.
Recovery reads bounded keyset batches instead of retaining every attempt body.
Row and encoded-metadata totals are initialized from durable tables once at open,
then maintained by SQLite triggers in each transaction, including rollback and
retention. Capacity checks and row replacements read those totals without
rescanning unrelated bodies. Attempts have a covering task/id index.
The active-attempt mutex protects only bookkeeping and task admission; durable
claim I/O runs without holding it. A tracked dispatch reservation keeps shutdown
waiting through claim, cancellation-token publication and worker registration.
Idle or fully occupied claim polling performs only indexed reads; it neither
starts a write transaction nor checkpoints. Claim materializes one queued row at
a time, retiring expired rows before selecting live work.
Admission commits the floor and outcome once. A rejected admission rolls back
its candidate changes to a savepoint while still persisting the advanced floor.
All matching rules for one authenticated delivery share that transaction and
savepoint; a failure never leaves a partially admitted rule set. Delivery/event
headers are unsigned correlation metadata. Replaying an identical signed body
with a new delivery name returns its existing receipt without allocating aliases;
a deployment-status body with a mismatched event label is rejected explicitly.
API mutation identities are scoped to the authenticated principal before reaching
the Ledger. Its explicit clock inputs must be valid positive Unix milliseconds
within the supported RFC3339 range; callers own the trusted clock source.
Async product paths dispatch SQLite/fsync/PNG work to retained blocking workers;
one async admission lane dispatches at most one Ledger worker at a time. Waiting
callers occupy no blocking thread; abandoning a caller before dispatch cancels
its operation, while a dispatched worker retains the lane through settlement.
Retirement closes the lane, refuses waiting callers and awaits dispatched work
before releasing leases.
Claim, linked cancellation-token registration and task dispatch share the Ledger
lane with cancellation. Cancellation commits first and cancels the registered
token before acknowledgement. Workers check cancellation before browser launch
and exploration. An acknowledgement disarms future stages; it is not a quiescence
receipt or rollback of effects already admitted. Abnormal worker destruction
removes its reservation and fences the Ledger rather than assuming convergence.
Poisoned live-attempt bookkeeping also fences the Ledger. Teardown recovers the
retained entries to cancel and remove them without panicking again.
Artifact reads bound the SQLite blob before allocation and revalidate canonical
PNG pixels and bytes. Corrupt durable evidence fences this Automation owner.
Settlement normalizes and validates every screenshot before the transaction, so
rejected external PNGs neither commit a verdict nor fence healthy storage.
Failure to durably settle an admitted execution fences storage and rejects new
admissions. It never releases durable execution accounting by assuming effects
ended; restart recovery disarms those interrupted rows without replay.
The listener retries accept failures with bounded, cancellation-aware backoff.
Results live for 30 days; screenshots live for seven. Retention maintenance runs
independently of Browser readiness or occupied execution slots. Storage fencing
remains authoritative for maintenance too. Protected Session labels and grant
scope identities are independent of those lifetimes. Retired rule scopes
retain View grants for historical Sessions; they cannot Cancel or Resume.
Metadata pressure stops further dispatch. Cancellation can use the reserved
settlement headroom while admission is full, but its complete receipt and state
change still respect the absolute metadata limits in one transaction.
Maintenance interrupts queued attempts after their 30-minute lifetime even when
Browser is unavailable. Cancellation changes queued/running checks, or a failed
check with an unfinished authorized exploration. Cancelling exploration preserves
the fixed check verdict; a completed failed check is not relabelled cancelled.
Cancelled exploration is terminal: late start, progress or completion callbacks
cannot overwrite the operator disposition.

The versioned API is `automation/*/1`. Local operators administer rules and
grants; Devices need separate per-rule view, cancel and resume permission.
Authenticated transport identity alone grants none. Protected Session reads,
history search, export, references and streams use the same source policy.
Public protected handles are read-only, including Local handles; only the typed
Automation owner controls their Goal. Unknown scopes fail closed. Revocation
ends reading leases; it does not abandon already admitted mutation settlement.

Default bounds are two running attempts, 32 queued attempts, 30 minutes queued,
ten minutes executing, two exploration rounds and 50 rows per API page. Per
round: 120 seconds, eight provider attempts, 16 Tools, 256 generated records and
1 MiB generated bytes. Readiness is observational and exposes coarse states;
local diagnostics own paths and dependency details. No GitHub writeback occurs.

Verification uses isolated signatures, deterministic Agent providers and a
nonserializable loopback fixture capability. Native Chromium, shutdown/SIGKILL,
Web/Desktop presentation and real provider checks are separate, explicit tests.

The listener accepts only bounded HTTP/1.1 POST requests on `/github/SOURCE`.
Malformed requests receive 400, oversized requests 413, and unavailable durable
admission 503.
The 32-connection admission bound also returns a bounded 503 JSON busy response.
Exploration always closes its preview and settles failure or cancellation before
returning an execution error; cleanup failure never overwrites the fixed verdict.
Checker cleanup follows the same rule: a complete result and its evidence settle
before reporting cleanup failure, which prevents a new exploration and remains
visible in the bounded evidence diagnostic.
Exploration completes only with a report bound to a canonically Completed source
Turn of the exact issued Goal. Complete, blocked and pause reports may describe
that verified investigation; a failed or unsettled Turn retains its unverified
claim as diagnostic evidence and settles exploration as Failed.
Policy snapshots share an immutable generation; only explicit edits copy it.
A separate private log keeps the latest 256 rejected intake
coordinates and closed reason codes, without bodies, signatures or secrets.
Rejections update a bounded in-memory journal; the scheduler coalesces writes
once per second on a retained blocking worker and shutdown flushes remaining
entries. A crash can lose entries since the last successful flush. This journal
is diagnostic; admission receipts remain immediately durable. Policy publication
serializes writers separately from permission readers; readers retain the previous
generation until publication succeeds or an uncertain publication fences access.
Snapshot and permission admission recheck the fence while holding the generation
lock; a reader queued before uncertain publication cannot mint a later lease.
Local diagnostics reports this log and any log-write failure. Logging is diagnostic and cannot make a failed admission successful.

GitHub does not retry failed deliveries automatically. The operator reads Local
Diagnostics, corrects readiness/capacity/policy, then uses the repository webhook's
Recent deliveries page to redeliver the recorded delivery. A rejected old event
cannot bypass the acceptance floor; initiate a fresh deployment instead. A 202
with `capacity_rejected` records a terminal attempt; use manual Resume after
capacity recovers. Results remain in RSI; there is no GitHub status writeback.
