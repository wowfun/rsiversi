# Host lifecycle

One owner process holds the standard Host paths at a time. A foreground daemon
publishes a same-user Unix-domain socket; an application uses it when its exact
protocol, product build, Host launch key, and Host epoch handshake is
compatible. Durable metadata remains structurally readable across executable
rebuilds so lifecycle commands can identify and signal an older exact process
generation; compatibility is enforced during application selection and the
handshake. The active daemon's validated metadata endpoint is authoritative,
including when the client's runtime-directory environment differs or cannot
itself hold a Unix socket. With
no owner, an application may acquire the same owner lease and
run a private embedded Host without publishing an endpoint. A starting,
embedded, or temporarily unresponsive owner is waited for up to the same
15-second readiness bound and is never bypassed by a second Host. The standard
product daemon is Linux-only because its lifecycle
signals are fenced by a pidfd plus Linux process start identity; other
platforms support embedded mode only.

The standard preset includes the pure [Goal domain](../../../rsi-agent/goal/README.md).
The separate Host [Goal controller](../../goal/README.md) runs only after an explicit
Session create/resume control. Reading a Goal or attaching a client leaves it
disarmed. The Session owner depends on that controller, whose cleanup retains
the Kernel until automatic inputs have been discarded or settled.

`RunningRsi::shutdown` reports completion of admitted Runtime teardown, not the
destruction of caller-owned Rust values. A cloned Local capability is an owning
Arc: a retained Session service can retain its Store, composition source and
native cache lease after its operations have been retired. In-process callers
must drop Session handles and cloned Local services before reopening the same
Host paths. A clean result does not authorize bypassing those leases. Application
cache slots isolate concurrent application owners; they are not a fallback for a
second Service owner. Runtime teardown cannot revoke arbitrary Rust ownership.
