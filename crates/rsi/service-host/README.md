# rsi-service-host

This package owns native process ownership and local API publication for the
standard product. One HostOwnerLease protects the canonical standard HostPaths
for both embedded and daemon execution. The standard daemon is Linux-only,
where exact process-generation signal fencing is implemented. Other platforms
remain embedded-only.

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

`LocalApiFactory` publishes an owner-only Unix listener for the
[shared API HTTP codec](../../rsi-api/http/README.md). It requires the owner lease,
API dispatcher and connection description. Startup supplies a validated launch
key; the product combines it with the exact running executable build into the
opaque LocalCompatibilityKey. The native peer UID supplies authentication.
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
connection and finite/binary/SSE decoders as the other transports. The product's
exact executable/launch gate remains stronger than remote wire negotiation.
Readiness completes the bounded description and operation-catalog exchanges,
without creating or attaching a Session.

The persistent lease, strict owner metadata and detached log live below
`<state>/session-host/`. Owner metadata schema 2 records EndpointId, HostEpoch,
process start identity, mode, launch key, executable build and the absolute daemon
endpoint. Schema 1 remains structurally readable for lifecycle control of an
older process; it cannot supply a current API connection identity. Compatibility
checks are separate from structural validation, so a rebuilt lifecycle client
can still stop an older exact process generation. Linux signals open a pidfd
and recheck the recorded process-start token before signaling that descriptor.

With XDG_RUNTIME_DIR, the socket lives under the private
`rsi/<state-root-digest>/` directory; otherwise it uses the protected owner
directory. The published absolute endpoint remains authoritative when a client's
runtime-directory preference differs or could not hold a bindable socket path.
Metadata discovery therefore does not require a usable client runtime directory.

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
