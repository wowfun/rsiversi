# rsi-jobs

This package owns runtime-independent process-local Jobs contracts. A job task
receives a cooperative cancellation token and returns one bounded JSON value.
Handles expose latest status and a joinable terminal outcome.
Failure outcomes retain at most 4 KiB of UTF-8 diagnostic text regardless of
the size of an error returned by trusted task code.

Callers may attach a generic bounded owner scope to a submission. Closing one
scope rejects racing work for that owner, cancels and joins only its unfinished
jobs, and never disturbs jobs owned by another scope or unscoped process work.
Agent compositions map the exact session and turn identities into this generic
scope; Jobs itself does not depend on Agent concepts.

`cancel_all` closes admission for its complete snapshot window, requests
cooperative cancellation for every unfinished job, and joins under the
provider-owned finite bound. A timeout is typed and leaves the still-running
task tracked for later settlement or plugin retirement. Global admission, or
the exact owner scope, remains closed until that timed-out snapshot settles;
unrelated owner scopes remain available.

The package contains no executor, persistence, retry policy, process spawning,
or plugin lifecycle.
