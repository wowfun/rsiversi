# rsi-extension

A native attachment can observe process disconnection with a child
cancellation token. Cancelling that attachment token does not stop the peer.
The connection retains its first terminal error and the operation that detected
it. A background application can report this cause when its process disappears.

After the version-1 handshake, the factory requires `extension.ping` to return
`null` every five seconds, within three seconds. A stalled or failed peer is
terminated and all pending operations fail; no callback is replayed. This is
liveness detection, not CPU or memory confinement. Graceful teardown uses the
protocol before native termination, including on Windows.

An extension connection owns one ManagedDuplexProcess. Its independent reader
and writer allow replies and callbacks while other requests await user input.
The protocol is newline-delimited UTF-8 JSON, at most 1 MiB before the newline.
Each frame contains the exact generation and a request, reply or cancellation.
Request IDs increase independently in each direction; both sides allow at most
64 outstanding requests and bound output queues. IDs are safe JSON integers.
Unknown fields, malformed frames, unknown replies, duplicate incoming IDs,
overflow and unexpected EOF close the entire connection. Calls are never replayed.

Caller cancellation sends a cancellation frame but retains the correlation slot
until the peer replies. A peer that does not settle cancellation within five
seconds loses its connection. Inbound handlers receive a cancellation token;
their result is discarded after cancellation. Dropping the connection closes
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
