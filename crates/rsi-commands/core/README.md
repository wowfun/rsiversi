# rsi-commands

This ordinary plugin owns one exact-name command registry. Registration leases
control visibility, descriptor order is deterministic, and dispatch clones the
handler before awaiting it. The plugin does not inspect chat messages.
Withdrawal removes visibility under the registry lock and destroys the removed
handler after unlocking, so a handler may own another registration lease.
Meta withdrawal removes service discovery; an escaped typed registry handle
remains usable, and its registration leases still control command visibility.
This follows Meta's [local-service lifetime](../../rsi-meta/core/README.md).

Execution lasts until the handler completes or the caller's cancellation token
fires. Effect sites own policy deadlines; the registry does not return early
from a non-cooperative in-process handler while work remains unsettled.
