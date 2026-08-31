# rsi-jobs

This package owns runtime-independent process-local Jobs contracts. A producer
generation accepts a bounded type-erased request and returns a live control
object. Jobs performs scope, producer, and capacity preflight before calling the
producer and assigns a job identifier only after the control object is owned by
the registry. A racing scope revocation or producer retirement cancels admitted
work instead of publishing it under stale authority.

Callers identify a scope with a bounded `JobScopeId`, but scoped operations
require a cloneable opaque `JobScopeAuthority` minted by the live Jobs provider.
The authority is process-local, non-serializable, generation-bound, and shares
revocation state with every clone. A later same-identity scope is a distinct
generation rather than a permanent tombstone.

The registry exposes `list`, `get`, raw offset-based `read`, `wait`, and `kill`
operations. One exact-scope list contains at most 256 records, aligning the
generic provider contract with bounded Tool JSON consumers. Reads of active
work do not report it. A terminal `read` or `wait` re-samples both final stream
tails before reporting. The active/terminal decision and active summary are
observed under one registry lock; a terminal observation always takes the
re-sample-and-report path instead of combining a stale stream snapshot with a
terminal summary. `kill` reports only after cancellation has settled. Reporting
drops producer control and captured output, retaining only a bounded tombstone.
Terminal jobs requiring a report are never evicted before that report. Reported
tombstones are evicted oldest-first within explicit scope and provider bounds.
A `Completed` terminal represents successful work: it may carry no process exit
code or exit code zero, and it cannot carry a terminating signal. Nonzero exits
and signals must be classified as `Failed` unless cancellation owns the
terminal classification.

Scope finalization synchronously revokes admission, snapshots unfinished work,
requests cancellation, and returns a report containing every terminal job that
still required reporting. Waiting and reaping belong to the provider: timeout
or cancellation of the finalizer future cannot abandon admitted work.

The package contains no executor, persistence, retry policy, process spawning,
shell policy, or plugin lifecycle.
