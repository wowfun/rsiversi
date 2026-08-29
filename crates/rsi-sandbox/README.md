# rsi-sandbox

`rsi-sandbox` defines file-effect sandbox modes, process plans, and truthful
enforcement stamps. [`rsi-sandbox-local`](local/README.md) is an ordinary plugin
that behaviorally probes explicit Linux bubblewrap candidates first and
explicit Landlock runner candidates second.

Restricted calls fail closed without a selected backend. The
`danger-full-access` mode is an explicit holder bypass and is stamped as
unconfined. This family builds process plans; the process owner remains
responsible for spawning, cancellation, output bounds, and recording the stamp
in Agent facts.

Explicit backend candidates are opened without following a final symlink,
must be regular files, and are copied from the pinned handle through a fixed
byte ceiling before any probe runs. FIFOs, devices, and oversized candidates
cannot block or fill staging storage.

`read-only` and `workspace-write` describe file writes, not secrecy or network
policy. The current local plans retain a read-only view of host paths and host
network access; their enforcement stamps therefore never claim filesystem
confidentiality or network restriction.
