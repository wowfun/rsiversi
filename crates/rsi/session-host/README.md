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
connection multiplexing. A readiness connection sends a side-effect-free
`Probe` request and requires `Ready` on that same stream under the control-plane
deadline; a successful handshake alone is not readiness. The client adapter
canonicalizes draft workspace paths before transport, and the server rejects a
non-absolute wire path rather than resolving it against the daemon's working
directory. Message text is checked against the durable message contract before
the in-process application is called. Every locally defined tagged frame,
operation, response, item, update, and error rejects unknown fields recursively
rather than silently accepting a shape from another protocol epoch. The server rejects an empty or oversized
content-block list before reading any declared upload body. Multimodal message
uploads remain strict JSON: the request declares each image length and SHA-256,
followed by ordered bounded base64 chunk frames. The server admits aggregate
decoded bytes before retention and verifies length and digest before calling
the Session module. No raw frame subtype or partially imported message is
exposed. A connect or handshake failure before message transmission remains an
ordinary backend failure. A read failure after a complete request can be
applied, or a matching response envelope that cannot prove exactly one result,
is reported as an unknown message outcome. Rejected upload framing is returned
as a typed response before application execution. Subscription
events carry one control record or Fact per frame. Message receipts carry both
the acceptance control cursor and their observed durable Fact tail, allowing a
claim wait to use one subscription instead of opening a handshake connection
for each status poll. One absolute one-minute
deadline covers the complete upload stream, so frame progress cannot renew an
upload reservation indefinitely. The decoded request keeps its raw-frame
admission until dispatch and response complete. A subscription releases that
charge before entering its unbounded event loop; only bounded identifiers and
cursors remain. Each upload reads one frame at a time using a separate 80 KiB
per-connection allowance, bounded to 10 MiB by the 128-connection ceiling.
It never reacquires the pool already charged for its retained request.
History Facts and subscription
events carry the requested Session identity, which the client verifies before
exposing them; receipt sequences, per-stream cursor continuity, monotonic
durable watermarks, and recent-session ordering are revalidated at the client
adapter boundary. History, recent sessions, and tree-wide pending approvals use a
start frame, one typed item per frame, and an end frame. Each item has the
single-frame byte ceiling and clients reject more than 1,024 items in one
sequence, including from a malformed same-user server. Clients additionally
enforce the requested operation's item variants, identity, count, and aggregate
page-byte bound. Ordinary frame lengths are admitted against one process-wide
64 MiB raw decode budget before allocation; handshake frames have a separate
16 KiB ceiling. Upload frame scratch follows the separate bound above.
Handshake, request-frame, response-frame, and write waits are bounded. An established
subscription may remain idle until an event or shutdown, but after its next
frame's first byte arrives, the remaining length prefix, decoder-ledger admission,
and body read share a 30-second deadline. Unpublished drafts have a one-hour idle
lease on their exact composition pin; each operation renews that lease, and a
targeted lookup removes only that draft when its deadline already elapsed. A
one-minute server sweep performs the global reclamation of expired pins and
bounded draft slots. Existing-session operations never scan all drafts;
creation also performs a bounded expiry pass before and after the potentially
long application call so expired drafts cannot consume capacity or be retained
across that call. Draft capacity is reserved before application creation and released whenever submit
establishes durable identity, including a submission conflict. Completed
connection tasks are reaped while the accept loop remains live. A cloneable
diagnostics handle exposes monotonic, saturating event and connection counters
for accept, peer identity, capacity, handshake, request, response, task-panic,
and forced-drain failures. The transport does not print or retain wire payloads;
the standard daemon periodically emits only nonzero counter deltas and a final
delta through its owner log. The
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

Protocol epoch 5 carries immutable input delivery, atomic Session inspection,
pending questions, question answers, and completed-output pages. Inspection and
questions use one bounded response frame: 256 requests of at most 64 KiB fit
inside the frame ceiling. Clients revalidate request/answer identity, inspection
cursors and activity, and output byte cursors before exposing the response.
History and approvals retain their existing per-item framed sequences.
