# rsi-execution

The product supplies `ExecutionResolverContract` at authenticated ingress. Its
resolver accepts a trusted `CallOrigin` and returns one opaque lease for the
requested location. Consumers pass that lease through their runtime ownership;
they cannot reconstruct it from durable coordinates or an approval display.
Bounded metadata reads use the same live admission without requiring a connection;
viewing an authorized offline history never reconnects a machine implicitly.
Metadata enumeration receives one admitted location selection. The selection is
bounded correlation data, while its separate owned operation retains all current
grant gates through the finite query. Consumers filter Store indexes before
pagination, newest cursors and complete-membership budgets; filtering the returned
page is insufficient. A later request must acquire a new selection.
Local and SSH use the same lease interface. Missing providers reject without
falling back to the Service filesystem. Resolver publication does not select
programs or transfer the Service environment to another machine.

An `ExecutionProvider` freezes one location, target revision, connection epoch and
backend generation. Composition supplies its current Host epoch, which is retained
in every provider, lease and plan identity. Process-local counters alone cannot
distinguish evidence issued before and after a Service restart. Each issued
`ExecutionLease` also pins its admission owner.
It is process-local authority and has no deserializer. The backend groups Sandbox,
batch and duplex Process, PTY, Files, path and program/environment resolution.
No operation looks up a replacement provider by location after preparation.

A resolved program also retains the exact lease and its complete environment.
Preparing a foreign program fails before backend I/O; equal executable paths do
not merge different selector environments.

A prepared process retains the exact lease, a complete target-selected environment
and a move-only backend payload. Environment substitution is rejected before I/O.
Consumption compares the lease's unforgeable in-process seal before admission or
backend I/O. A plan from another lease is rejected even when its public location
and revision fields are equal. Its identity exposes only immutable correlation
metadata and a monotonically assigned sequence for approval evidence; those
values cannot reconstruct the plan or grant permission to execute it.

Spawn admission transfers its owned permit to a retained task before calling the
backend. Its publication guard retains the permit until the caller consumes the
verified handle, or until an unpublished child is terminated and joined after
reply loss. Abandonment synchronously requests termination even outside the
creating runtime; joining stays on that runtime and retains admission. The runtime
owner must drain accepted work before shutdown to guarantee reaping. A long-lived published child does not keep a start operation open.
Its separate `ExecutionPin` retains the provider tuple through actual settlement.
The returned byte ports check current admission before new reads, writes and
resize. A stream read checks authority again before publishing a received chunk, without
reserving operation capacity at either check or during an idle wait; otherwise a silent terminal
would prevent revocation from draining. Accepted writes, resize and Files
operations retain their permit independently of the waiter. Files views retain
the pin and check admission for each access.
Each lease scopes incoming Files callers to private opaque identities shared by
that lease's views. At most 64 caller mappings are retained until caller release.
Equal application bindings in separate leases cannot continue or release each
other's tokens. Releasing a caller affects only that lease's private identity.
Final lease release withdraws remaining private caller identities after retained
operations have relinquished their pins.
Each resource admits at most 64 private caller identities. Pending opens reserve
an identity through backend settlement and reply consumption. Failed or abandoned
opens release an otherwise unused identity; concurrent opens and already published
tokens keep their caller mapping until explicit caller or resource release.
An explicit `ExecutionFiles` resource may retain that private scope beyond its
creator lease. It grants no read access: each operation view requires a current
lease from the exact same provider object and checks that caller's admission.
Accepted operations retain both the resource and the current operation permit.
Dropping the last resource/view releases its private caller identities; dropping
an individual view does not invalidate another view's tokens. Replacement
providers cannot consume existing resources, even at equal filesystem paths.
Termination and settlement remain available after revocation. Native plan owners
retain the pin alongside Sandbox resources; remote providers bind it to their
connection epoch. Backend-owned handles never choose a new connection.

Admission is supplied by the product's grant owner. Scope admissions retain whole
Tool, model, review or transfer lifetimes; operation admissions retain individual
backend effects. These use independent bounded pools. Publication checks retain
current authority but consume neither pool: they perform no backend effect and
must never discard consumed bytes because an unrelated operation filled a pool.
Capacity rejects work before its effect and callers may retry that admission;
unknown outcomes never authorize replay. Each effect receives an owned permit; dropping a lease or returning a prepared plan is not a new
grant. Coordinates and approval display metadata are not access authority.
The core does not implement SSH, target configuration, credential resolution,
Session scheduling, or its own retry policy.

Local contributions may supply their own frozen executable and complete environment
through `resolve_local_program`. The lease checks Local before calling the pinned
backend, which performs native validation and seals the result to that same lease.
This preserves independently configured native plugins without selecting another
Process or Sandbox. SSH rejects this Local operation before backend I/O. Target catalog lookup and
explicit target configuration below are separate operations.

A retained terminal uses `ExecutionPty` to separate resource lifetime from the
current caller. Its creator authorizes spawn; the token pins that provider and
owns bounded private output draining and cleanup. It cannot write or resize by
itself. An operation view requires a live lease from the exact same provider
object, including Host/target revision/connection epoch. Another authorized
caller can therefore control the same terminal without borrowing its creator's
grant. A replacement provider cannot consume the old token. Private output is
accepted terminal work; the product must authorize every publication of that
bounded projection. Dropping the resource requests termination; confirmed cleanup
still requires settlement.
Operation views expose only write and resize. Releasing a view releases its caller
pin without terminating the retained terminal resource.

A retained duplex server similarly separates process ownership from each bounded
protocol exchange. `ExecutionDuplex` owns private output draining and cleanup;
`exchange` compares the exact provider object and acquires the current caller's
operation before returning a writer. That writer admits at most 1 MiB plus one
framing newline for 30 seconds
and retains the operation through accepted writes, including abandoned waiters.
The deadline runs independently of writes. Expiry retires and settles the server
before releasing the exchange's admission, including an idle retained exchange.
An in-flight write uses the same absolute deadline and retains its own operation
until timeout cleanup settles; expiry never transfers authority to a new writer.
Revocation prevents another exchange; an already accepted exchange may settle.
The protocol owner retains its exchange until a verified response or controlled
retirement, bounds its private receive queues, and separately authorizes publication.
The resource exposes no general input port. Its separate bounded protocol-reply
operation belongs to accepted server maintenance, like output draining: only the
trusted protocol owner may send its fixed acknowledgement or unsupported-request
response. It must never carry a caller's business request. That owner limits reply
queues; each write is at most one Process chunk and 30 seconds, followed by native
settlement on timeout. Business exchanges cannot borrow this maintenance path or
the creator's grant. Replacement providers cannot address the old child.

`prepare_source_reader` explicitly requests the pinned Sandbox source-reader view.
It requires ReadOnly pipes, read-only Host scratch and isolated networking, while
retaining the same program seal, admission and provider as ordinary preparation.
Source owners use it when repository ancestors in host scratch must remain
visible. Ordinary Tools and PTYs continue to request ordinary confinement.

Trusted contribution owners may explicitly select a remote command and bounded
extra environment with `resolve_target_program`. This separate operation rejects
Local leases before backend I/O. The target resolves an absolute target path or
basename using its own account and fixed PATH; Service environment snapshots are
never accepted implicitly. Callers own configuration and credential-export policy.
The returned program remains sealed to the exact original lease. The [SSH helper contract](../../rsi-ssh/helper/README.md) owns its connection
selection and aggregate byte bounds; equal selections reuse their frozen resolution.
