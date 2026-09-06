# rsi-process-local

A failed removal retains its byte reservation and disables new cache captures
until the next successful open. This prevents filesystem cleanup failures from
silently recycling file or byte quotas. Process execution and pipe draining
continue with unavailable full-output references.

The local provider spawns each process as the leader of a new Unix process
group, drains stdout and stderr concurrently into exact-capacity byte tails,
settles stdin delivery while the managed group is live, aborts an incomplete
writer after that group is gone, and reaps the direct child. A leader that exits while
descendants remain causes the provider to close the group before publishing a
terminal outcome. `terminate` is idempotent and sends TERM to the managed
group, waits the caller-supplied grace, then sends KILL if the group is still
live. Provider retirement closes admission, waits for every in-flight spawn to
publish into provider ownership, starts termination for every live group, and
waits for complete group settlement under a finite provider bound.
If that bound expires, provider retirement reports the timeout while its
detached cleanup task retains the sole service and child ownership through
TERM, delayed KILL, pipe-task joins, direct-child reaping, and output-cache
shutdown. The timeout is
therefore an honest lifecycle failure, not permission to abandon the managed
group.
Registry retirement, liveness probes, and TERM/KILL delivery are
identity-checked: a late completion or timer for an older process never
observes, removes, or signals a newer managed owner if the operating system has
reused the same numeric PID. A permission-denied group probe still means that a
group exists; it is not treated as clean settlement.

Once the direct child is reaped, group disappearance is also finite. A member
that survives TERM, the request grace, KILL, and 10 seconds of post-KILL
observation makes `wait` return `SettlementTimeout`; the provider releases the
active slot only after aborting and joining its pipe tasks. This is an honest
failure, not a claim that the kernel removed an unkillable task.

On non-Unix targets the package compiles but rejects spawn as unsupported. The
provider does not claim to contain a descendant that deliberately leaves the
managed process group; restricted Bubblewrap execution provides the stronger
PID-namespace boundary when native behavior tests establish it. The same
boundary distinction applies to abrupt host death: Bubblewrap plans use
`--die-with-parent` and a PID namespace, while an unconfined process group has
no process-external supervisor and therefore no crash-containment guarantee.

`output_cache` optionally configures a private completed-output directory.
Invalid configuration is rejected before activation. A valid configuration whose
directory cannot be opened disables disk capture for that generation while
keeping Process and the read-only cache interface available; reads report
the original cache-open failure. Cache I/O availability never becomes command-spawn authority. The
standard product places it under `HostPaths.cache()/process-output/v1`.
Capture defaults to 64 MiB per stream, 512 MiB total, and 256 files (total hard
ceiling 1 GiB). One bounded worker writes at most 8 MiB of queued/in-flight
chunks. Pipe draining never awaits disk writes: saturation, stream overflow,
or I/O failure abandons that stream's complete file and preserves its tail.
Failed pipe settlement abandons both captures before publishing the outcome,
so retained process handles cannot keep failed disk reservations pending.
Completed entries are FIFO-evictable; active reservations cannot be evicted.
At EOF publication waits at most 250 ms. A timed-out capture cannot publish
later, and its worker retains file/lock/quota ownership until actual I/O ends.
The worker releases its writer lock explicitly after all I/O settles and before
acknowledging shutdown; duplicated descriptors cannot extend that lease.
Shutdown closes admission and records a worker stop request independently of
queue capacity. The provider's configured shutdown wait may expire, but once blocked I/O returns the
worker drains admitted commands and releases its lease even while cache or
process handles remain alive. Cache cleanup has no earlier independent deadline;
repeated shutdown waits observe that same completion.
Normal exit preserves completed files. A bounded startup scan admits only
owner-owned, no-follow regular files with generated names and deletes its own
partial files. This cache makes no fsync or crash-durability promise.
Generated private regular entries with multiple hard links are unlinked from
the cache during startup, including interrupted `.part` to `.log` publication.
Only cache-local names are removed; external link names are never followed or
modified. A surviving single-link completed file may be admitted normally.
