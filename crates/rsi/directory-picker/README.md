# rsi-directory-picker

This ordinary Service plugin owns product directory browsing and single-level
creation. The [API contract](../directory-picker-api/README.md) owns its external
bounds. Composition supplies an optional Host home; activation does not inspect
ambient browser or worker paths. Both listing and creation hold a ConfigurationAccess
lease because they expose or modify Host filesystem state beyond registration.

Unix resolves directory aliases to physical paths, then acquires retained
no-follow directory handles. Enumeration and single mkdir use the shared native
Files mechanics. Creating holds the parent handle and verifies its identity against
the returned physical path. Replacement after a successful mkdir reports an
unknown outcome; no rollback can safely delete that name. Entries with names or
resolved targets that cannot be expressed as UTF-8 are skipped with a notice.

At most two actual blocking filesystem tasks run. Each caller waits at most five
seconds; dropping a caller or reaching its deadline stops scheduling new I/O.
Uninterruptible syscalls retain the actual task slot and grant lease until they
return. Retirement closes admission, cancels further I/O and waits for those
actual tasks. A read timeout is explicit; creation timeout is outcome unknown.
There is no automatic creation retry. Non-Unix builds register status and return
Unsupported without requiring a native implementation.

The factory's configuration echoes its captured `home` (including null) exactly.
The standard API fragment records this value so the frozen Service composition
identity changes with its default browse root. Profiles cannot silently substitute
an ambient home behind that identity.

Status reports `allowed: false` only for an authorization denial. Exhausted
configuration capacity and shutdown remain their explicit API errors, so a
temporary admission failure never masquerades as a missing durable grant.
