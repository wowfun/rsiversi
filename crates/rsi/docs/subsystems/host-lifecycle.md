# Host lifecycle

One owner process holds both standard config and state homes. Ordinary Windows
and Linux applications start or attach to a resident Host through a startup
coordinator. The Host publishes a same-user endpoint: a private Windows named
pipe or Linux Unix-domain socket. Applications negotiate the interfaces they
use and bind to the active Host epoch. An optional package or rebuilt client
does not redefine the running Service's identity.

Validated active metadata selects the endpoint, including when a client's
runtime-directory environment differs. Frozen Lifecycle v1 supports status and
stop across product protocol changes. Native user, PID and process creation
identity fence the operation. The Host executes an immutable SHA-addressed copy,
so developers can rebuild the original binary while clients remain connected.
Starting, embedded or unresponsive owners are never bypassed by another Host.
Explicit SDK embedding remains available under the same exclusive leases.

Closing an ordinary application releases its attachment. It does not stop the
Host or other attachments. Reset and import require stopped home owners; core
upgrade coordinates an explicit stop, verified replacement and start. Their
recoverable operations and final-home options belong to the
[launch contract](../../core/docs/launch.md).

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
