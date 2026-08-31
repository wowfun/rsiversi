# rsi-sandbox

This package owns platform-neutral sandbox modes, explicit process inputs,
confined process plans, enforcement stamps, and the Local service contract. It
contains no feature probing, process spawning, filesystem mutation, or plugin
lifecycle.

An enforcement stamp is a closed semantic combination. Unconfined evidence is
valid only for danger-full-access with host scratch and network; Bubblewrap
evidence is restricted, uses private `/tmp`, and currently retains host
network; Landlock evidence is restricted, uses host scratch, and currently
retains host network. Unsupported or contradictory combinations are rejected
when durable data is decoded or revalidated.
The stamped workspace is also revalidated as an absolute, lexically normalized
path: relative paths and paths containing `.` or `..` components are rejected
without requiring the durable path to exist or re-canonicalizing the live
filesystem. A restricted stamp cannot name `/` or `/tmp` itself as the
workspace, matching the request boundary shared by all local restricted
backends.
