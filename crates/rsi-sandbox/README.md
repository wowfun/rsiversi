# rsi-sandbox

`rsi-sandbox` defines file-effect sandbox modes, process plans, and truthful
enforcement stamps. [`rsi-sandbox-local`](local/README.md) is an ordinary plugin
that behaviorally probes explicit Linux Bubblewrap candidates first and
explicit Landlock runner candidates second. Standard composition supplies no
Landlock candidate.

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
policy. Durable stamps identify the staged backend bytes by SHA-256 rather than
an ephemeral staging path and separately record filesystem, scratch, and
network evidence. Bubblewrap restricted plans use a private tmpfs `/tmp`; a
workspace exactly equal to `/tmp` is rejected in either restricted mode because
its later bind would erase that boundary. Bubblewrap also rejects `/` because a
later root rebind would erase its private `/tmp`, `/proc`, and `/dev` mounts,
while a workspace below `/tmp` is rebound after tmpfs creation. Current plans retain host network access and never claim filesystem
confidentiality or network restriction.
