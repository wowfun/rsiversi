# rsi-session-host

This package owns the standard product's same-user local Session Host control
plane. One `HostOwnerLease` covers both an embedded owner and a daemon owner for
the canonical standard `HostPaths`. The package provides a private framed-JSON
UDS adapter on Unix, while the standard product publishes an explicit daemon
only on Linux, where process-generation fencing is implemented. Other platforms
remain embedded-only.

Linux lifecycle signals open a pidfd for the recorded PID and then recheck the
recorded process-start token before signaling that exact descriptor, closing
the PID-reuse gap between validation and delivery.

The transport carries application/session operations, bounded request IDs, and
an exact protocol/build/launch-key/owner-epoch handshake. The product build is
the package version plus SHA-256 of the running executable, so separately built
artifacts cannot silently share compatibility identity merely because both
declare version `0.0.1`. Each connection performs one handshake and carries
exactly one request; request IDs correlate that exchange and do not imply
connection multiplexing. The client adapter canonicalizes draft workspace paths
before transport, and the server rejects a non-absolute wire path rather than
resolving it against the daemon's working directory. Submit-text bytes are
checked against the durable
turn-text contract before the in-process application is called. Subscription events
carry one Fact per frame. History Facts and subscription events carry the
requested Session identity, which the client verifies before exposing them.
History, recent sessions, and pending approvals use a
start frame, one typed item per frame, and an end frame. Each item has the
single-frame byte ceiling and clients reject more than 1,024 items in one
sequence, including from a malformed same-user server. Clients additionally
enforce the requested operation's item variants, identity, count, and aggregate
page-byte bound. Frame lengths are admitted against one process-wide 64 MiB raw
decode budget before allocation; handshake frames have a separate 16 KiB ceiling.
Handshake, request-frame, response-frame, and write waits are bounded. An established
subscription may remain idle until an event or shutdown, but after its next
frame length arrives, decoder-ledger admission and the remaining body read have
a 30-second deadline. Unpublished drafts have a one-hour idle
lease on their exact composition pin; each operation renews that lease, and a
one-minute server sweep reclaims expired pins and bounded draft slots. Draft
capacity is reserved before application creation and released whenever submit
establishes durable identity, including a submission conflict. Completed
connection tasks are reaped while the accept loop remains live. The
owner alone may recover a stale socket after a failed liveness probe. Published
sockets and their directories are owner-only, peer credentials must match the
effective user, and cleanup rechecks the bound device/inode.

The persistent lease, strict owner metadata, and detached log live below
`<state>/session-host/`. Owner metadata validation is compatibility-independent:
it bounds and validates the durable document so a rebuilt lifecycle client can
still identify and signal an older exact process generation. Protocol, product
build, launch-key, and epoch compatibility remain handshake and client-selection
checks. With `XDG_RUNTIME_DIR`, the endpoint lives below a private
`rsi/<state-root-digest>/` directory. The runtime root, `rsi` parent, and endpoint
directory must be real directories owned by the effective user; the Host never
follows a runtime-parent symlink or changes another owner's permissions.
Otherwise the endpoint falls back below the protected persistent owner
directory. Owner metadata records the daemon's validated absolute endpoint,
which remains authoritative when a client has a different or unbindable
runtime-directory environment; deriving the client's preferred endpoint is not
a prerequisite for reading that metadata. The daemon stages the socket,
publishes the same inode by hard link, and removes it only after device/inode
revalidation.

`ApprovalBroker` is the standard Host's one waiting approval answerer. Pending
requests are live process state rather than Facts. Every capable client attached
to the same Session may observe them; the first valid answer wins, later answers
observe settlement, and cancellation, Host shutdown, or dropping the waiting
answer future removes the request.
