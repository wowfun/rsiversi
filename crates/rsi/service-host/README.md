# rsi-service-host

Startup provenance may identify a validated installation id and canonical prefix.
The launcher registers a deferred resolver: admission runs once when provenance
is needed for Host creation/publication, not for ordinary command parsing or queries.
When launching an upgraded or recovered executable, admission uses that selected
source artifact rather than the parent process's old image.
It remains distinct from home authority and executable digest: two installations
can share both homes and a build. Lifecycle consumers preserve older metadata
without provenance and require explicit consent before assuming its origin.
Reusable graceful-stop coordination fences the process start token, never silently
escalates to force, and reports PID and operating-system recovery guidance.

This package owns native process ownership and local API publication for the
standard product. One HostOwnerLease exclusively protects both configuration and
state homes for embedded and daemon execution. Each home retains its own
`session-host/owner.lock`; changing only one home cannot open another writer for
the shared home. Acquisition orders canonical directory identities and releases
every partial acquisition on failure. Startup coordination uses distinct
`startup.lock` files under the same two authorities. The child acquires its own
owner leases before provider activation; no inherited standard stream acts as
an ownership capability. Linux and Windows provide resident daemon endpoints.
Native identity checks fence process generations; foreground embedded ownership
remains available to library embedders.
Home lease retirement explicitly unlocks before closing its file, so an unrelated
child paused between fork and exec cannot extend the parent's ownership through
an inherited close-on-exec descriptor.
The local-stream API compiles on Unix and Windows; resident product daemon
admission remains restricted to Linux and Windows.

Windows ownership creates a TokenUser-owned protected private directory
through Native Files, opens its owner lock without following reparse points, and
retains a namespace pin and exclusive file lease through provider cleanup.
Contention refuses another owner; compatible clients attach to its endpoint.
Deleting its lock is never a recovery action. Published metadata records the
canonical config/state authority pair. A reader selecting a different pair
cannot attach to or stop the owner merely by reusing its state home.
Embedded metadata publication flushes its staged file before replacement and
synchronizes the pinned private directory through Native Files on Windows.
It does not use a plain file open for a Windows directory or ignore flush errors.

Linux lifecycle checks require a matching process start token and exclude an
exited, unreaped zombie. A foreground owner's caller may reap it only after
`host stop` returns; retaining `/proc` metadata is not evidence that the service
is still running. Signals still use a pidfd and recheck the exact start token.

`ServiceOwnerFactory` publishes the actual lease and fresh HostGeneration before
durable providers activate. Process startup can inject its preacquired lease
and epoch; standalone compositions acquire explicit paths during activation.
Construction and Profile preview perform neither acquisition nor identity
generation. The owning Scope retains the lease through cleanup.

`ServiceIdentityFactory` depends on that owner and Storage. It publishes
EndpointIdentity from schema-1 Storage domain `rsi.service.identity`, with one
record and at most 128 encoded bytes. A new identity is durable before publication.
Malformed or conflicting records are rejected without replacement. The lease
serializes initialization and pins its first backend and endpoint for this
process. Initialization has an independent task owner and plugin cleanup drains
it, including when the activation waiter disappears. EndpointId survives restarts;
HostEpoch is generated for each running owner and is not stored in that record.

`LocalApiFactory` publishes an owner-only Unix socket or authenticated Windows pipe for the
[shared API HTTP codec](../../rsi-api/http/README.md). It requires the owner lease,
API dispatcher and connection description. Startup supplies a validated launch
key, from which it derives the opaque LocalCompatibilityKey. This key is neither
authentication nor a whole-build compatibility test. Native peer credentials
supply authentication.
The listener capability exposes its socket, diagnostics and stop result without
shutdown authority. Cancelling a stop waiter does not stop serving.

The listener retains its lease, stages the socket and hard-links the verified
inode into place. A live socket cannot be replaced; stale removal requires a
failed liveness probe and unchanged device/inode.
The probe is nonblocking: a full connection backlog still means a live owner.
A socket that disappears during stale cleanup needs no removal; a replacement
with a different identity is preserved and rejected.
Socket mode is 0600 and its private directory is 0700. Runtime roots and parents must be real directories
owned by the effective user. Cleanup rechecks the published inode and preserves
a replacement. At most 128 connection tasks remain live; completed tasks are
joined during ordinary serving. Meta cleanup cancels transport waiters, drains
or aborts their tasks within one minute, and removes the socket. The API registry
independently drains admitted mutations.

Domain endpoints register their own DTOs and limits. The listener contains no
Session operation switch, draft table or composition pin. The
[API contract](../../rsi-api/README.md) owns admission and mutation supervision;
[native UDS client](../../rsi-api/uds-client/README.md) shares the same negotiated
connection and finite/binary/SSE decoders as the other transports.
Readiness completes the bounded description and operation-catalog exchanges,
without creating or attaching a Session.

Executable SHA-256 is diagnostic artifact identity, available through
`service_host_executable_build`. Transport epoch checks precede negotiation;
owning APIs then compare generated protocol descriptors for the operations a
client consumes. Independently built executables can attach when their required
interfaces match. An optional Application catalog does not define home ownership.
Accepted Service source revisions are republished after Profile convergence
without changing the home authority, process epoch or existing attachment identity.

The persistent lease, strict owner metadata and detached log live below
`<state>/session-host/`. Owner metadata schema 3 adds admitted installation provenance to the schema-2
EndpointId, HostEpoch,
process start identity, mode, launch key, executable build and the absolute daemon
endpoint. Unrecorded binaries continue writing schema 2. Schema 1 remains structurally readable for lifecycle control of an
older process; it cannot supply a current API connection identity. Compatibility
checks are separate from structural validation, so a rebuilt lifecycle client
can still stop an older exact process generation. Linux signals open a pidfd
and recheck the recorded process-start token before signaling that descriptor.

Ordinary startup places the socket in the protected state home, so logging out
does not remove the resident Host's endpoint. Embedders may explicitly supply a
different runtime directory. The published absolute endpoint remains authoritative;
metadata discovery does not require a bindable client runtime directory.

The diagnostic handle combines native socket acceptance, peer identity,
connection capacity, service/task failure and forced-drain counters with the
shared [HTTP diagnostics](../../rsi-api/http/README.md). Each owner maintains its
own monotonic, saturating counters without retaining payloads. The standard
daemon emits nonzero anomaly deltas and a final delta to its owner log.

`ApprovalBrokerFactory` owns the standard Host's one waiting approval answerer,
its answerer registration, and the injected Session approval control. Pending
requests are live process state rather than Facts. Every capable client attached
to the same Session may observe them; the first valid answer wins, later answers
observe settlement, and cancellation, Host shutdown, or dropping the waiting
answer future removes the request.
The broker admits at most 1,024 requests and 16 MiB of aggregate encoded request
bytes per Host generation. It measures each request without a temporary JSON
buffer, retains that charge with the pending entry, and releases it on answer,
waiter drop, or stop. This is an encoded-data capacity policy, not an exact heap
measurement. Large prepared reviews therefore cannot multiply the per-item
4 MiB bound by the count limit.
Settled answers retain a bounded Host-generation receipt: an identical retry
succeeds, a different decision conflicts, and an evicted or cancelled request
is unavailable. Receipts are live transport evidence, not durable approval
Facts, and never recreate a request after Host restart.

`QuestionBroker` is an ordinary Base plugin scoped to one Host generation. It
admits at most 256 live requests and retains a separate FIFO of 256 settled
answer receipts. Requests and answers follow the
[User Questions contract](../../rsi-user-questions/protocol/README.md).
Cancellation, waiter drop, and Host shutdown remove pending questions; restart
never recreates them. Waiter cleanup removes only its own registration, even
after receipt eviction allows another request to reuse the identity.
Identical answer retries succeed while their receipt is
retained; conflicting answers fail.

Session interaction snapshots, subtree approval routing and immutable message
reads belong to the [Session API](../session-api/README.md). Workspace, Models,
Settings, Output and Media remain independent services. Media publication and
later message acceptance retain their separate failure semantics.

Persisted and selected Unix socket paths must be absolute and accepted by the
host standard library Unix address constructor, including its native length
limit and NUL rejection. Linux address width is not assumed on other Unix hosts.

The independent Lifecycle v1 listener accepts status and graceful-stop requests
for one exact HostEpoch before product API compatibility is considered. It uses
the same native peer-authentication boundary, a 16 KiB length-prefixed JSON frame,
a three-second exchange deadline and at most 32 concurrent connections.
`LifecycleFactory` is an ordinary plugin; it publishes a read-only stop-request
signal to the process owner. Acknowledging stop precedes cancellation of the
Service lifetime. Client disconnection does not itself request shutdown.
Lifecycle discovery reads a fixed bounded identity projection from `owner.json`:
PID, process start token, HostEpoch, mode, endpoint and canonical home authority.
Owner and Lifecycle readers share the same bounded process-token grammar:
native numeric tokens, or `process-PID` for embedded-only unsupported platforms.
Those fallback tokens do not enable native daemon liveness or signalling.
It ignores product fields and product metadata format revisions. The Lifecycle v1
response contains only this identity and shutdown status; it never embeds the
evolving product metadata schema. Product compatibility is a separate optional
status diagnostic. Force-stop remains native process-handle control.

`LocalServiceListener` lets a product plugin bind a named sibling stream while
retaining the same owner lease. It validates the native peer before returning a
stream; the plugin owns framing, bounded connection admission and shutdown.
Clients use the recorded owner identity and verify the live native server.

Offline purge holds startup and dual-home owner leases before endpoint cleanup.
The owner accepts only a validated, stopped lifecycle identity bound to the same
home authority. On Unix it probes and removes that record's product/control
sockets, never enumerating the surrounding runtime root. Unknown runtime entries
remain; missing legacy home authority does not authorize external cleanup.

Graceful stop allows the drain timeout plus 15 seconds for native exit, preserving
the CLI shutdown budget. Installation origin is admitted by the launcher and
resolved once only when a Host needs it; it distinguishes same-build installations
sharing homes and does not replace process-generation or home-lease authority.
Older owners lacking provenance require confirmation during ordinary uninstall.
