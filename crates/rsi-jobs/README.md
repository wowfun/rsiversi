# rsi-jobs

`rsi-jobs` owns process-local background job contracts. The
[`rsi-jobs`](core/README.md) package defines bounded task, status, outcome,
handle, and service seams. [`rsi-jobs-local`](local/README.md) is the ordinary
Tokio-backed plugin.

Jobs are live convenience work, not durable Agent turns or provider-managed
external jobs. Handles expose latest status, cooperative cancellation, and
join. The service also exposes bounded `cancel_all`: it temporarily closes
admission, snapshots and settles every unfinished job, then reopens admission
unless retirement has begun. Headless uses it in a pre-terminal Agent
finalizer. Plugin retirement permanently withdraws submission, then performs
the same bounded drain. No work is recovered after process exit.

The registry retains exact unsettled counts per scope. Releasing a timed-out
scope is therefore constant work for each completion rather than a scan of all
other jobs.
