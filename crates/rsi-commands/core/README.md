# rsi-commands

This ordinary plugin owns one exact-name command registry. Registration leases
control visibility, descriptor order is deterministic, and dispatch clones the
handler before awaiting it. The plugin does not inspect chat messages.

Execution lasts until the handler completes or the caller's cancellation token
fires. Effect sites own policy deadlines; the registry does not return early
from a non-cooperative in-process handler while work remains unsettled.
