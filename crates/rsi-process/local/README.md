# rsi-process-local

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
TERM, delayed KILL, pipe-task joins, and direct-child reaping. The timeout is
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
