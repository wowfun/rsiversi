# rsi-extension

A native attachment can observe process disconnection with a child
cancellation token. Cancelling that attachment token does not stop the peer.
The connection retains its first terminal error and the operation that detected
it. A background application can report this cause when its process disappears.

After the version-2 handshake, the factory requires `extension.ping` to return
`null` every five seconds, within three seconds of the ping being written. A stalled or failed peer is
terminated and all pending operations fail; no callback is replayed. This is
liveness detection, not CPU or memory confinement. Graceful teardown uses the
protocol before native termination, including on Windows.

An extension connection owns one ManagedDuplexProcess. Its independent reader
and writer allow replies and callbacks while other requests await user input.
The protocol is newline-delimited UTF-8 JSON, at most 1 MiB before the newline.
Each frame contains the exact generation and a request, reply or cancellation.
Request IDs increase independently in each direction; both sides allow at most
64 outstanding business requests plus one ping and one close in each direction.
Control capacity cannot be borrowed by business traffic. All frames share one
FIFO and monotonically allocated IDs; there is no priority reordering. The queue
holds at most 198 frames plus one active write: one request and at most one
cancellation per outbound slot, and one reply per inbound slot. Outbound slots
remain occupied until both the reply and their queued writes settle; inbound
slots remain occupied until their reply leaves the queue. IDs are safe JSON integers.
Unknown fields, malformed frames, unknown replies, duplicate incoming IDs,
overflow and unexpected EOF close the entire connection. Calls are never replayed.

`Connection::request` distinguishes a request rejected before writer-queue admission
(`RequestFailure::NotDispatched`) from a failure after admission
(`RequestFailure::MayHaveExecuted`). Encoding, capacity and cancellation failures
before admission prove that this request did not reach the peer. After admission,
transport errors alone cannot prove non-execution, even when no reply was received.
Adapters use this evidence to retire only their own unexecuted operation resources;
business receipts continue to determine publication and recovery.

The optional body codec carries bounded business values inline up to 256 KiB or
as immutable request-owned bytes. Paged descriptors declare their byte length
and an owner-issued token; fragments carry at most 512 KiB of bytes as base64.
Their 699,052-byte base64 payload leaves 349,524 bytes for the bounded JSON frame
metadata within the existing 1 MiB limit. This reduces callback count for large
bodies without changing retained-body admission; history and Step paging keep
their separate limits.
The business adapter supplies its byte ceiling, capacity permit, authorization
and request lifetime. Outbound encoding belongs to that owner; the Agent probes
the inline size without allocating serialized bytes and encodes only larger
values into its request-owned buffer. Core owns neither a transfer registry nor
persisted bodies. Body resolution deserializes directly into the caller's owning
type, so its boundary validation also applies to paged bodies without an
intermediate JSON value tree.

Caller cancellation sends a cancellation frame but retains the correlation slot
until the peer replies. A peer that does not settle cancellation within five
seconds after its cancellation frame is written loses its connection. Queueing
does not consume peer response time. Each frame write has a 30-second deadline,
so a peer that stops reading cannot stall the FIFO indefinitely. One
connection-owned deadline driver tracks ping and cancellation acknowledgements
and removes deadlines on replies. Shutdown closes
business and new-ping admission before using its independent close slot.
Inbound handlers receive a cancellation token. By default cancellation drops
their future. A handler may opt a method into `drain_on_cancel`: cancellation or
registration withdrawal signals its token and awaits its cleanup before replying.
Such handlers must terminate their work cooperatively; the existing peer
cancellation deadline still applies. Dropping the connection closes
admission and terminates the process. Explicit shutdown waits for protocol close,
then termination and Process settlement. No synchronous native ABI gate or MCP
exchange lock lies in the request path.

The ordinary factory consumes an exact executable, arguments, working directory
and complete environment from its caller. It selects trusted full-access Sandbox
execution explicitly. There is no executable discovery, credential inheritance,
DSH logic, package installation, or Store access in this core package. A generation
handshake must succeed before publishing the connection to other plugins.
Product factories may retain immutable launch resources with the connection.
Those resources are also held by a Process-settlement waiter, so cancelling
activation or dropping the last caller cannot release a running child's files.

Family adapters may register at most 64 exact inbound method names on the
connection. Duplicate registration fails. A registration owns admission and
cancels its in-flight callbacks when dropped; callbacks never inherit authority
from their JSON arguments. Unregistered methods use the host's original handler.
An Agent Profile that provides and consumes `rsi.extension.connection` groups
those leaves under fresh Local isolation for that key. This keeps simultaneously
live generations from publishing into the same slot; consumers inside each
group resolve the connection for their own generation.

The opt-in `native` target uses `RSI_EXTENSION_NODE` as an absolute installed
Node executable. Its DSH interoperability test additionally requires the absolute
`RSI_DSH_INSTALLED_COHORT` directory. It selects the Cordis transport and bridge
body codec by their package-relative paths inside that same installed cohort;
neither module's location is inferred by walking the other module's parents.
It exercises the actual peer through native pipes, including
partial writes, callbacks, cancellation, and heartbeat/close at 64 business calls.
The same installed cohort supplies the bridge body codec for a 1 MiB escaped
input, 3 MiB Unicode Tool arguments and an exact 8 MiB Application body.
The manual `extension-generations` workflow provisions that exact installed
source cohort and runs this interoperability test alongside Agent replacement.
