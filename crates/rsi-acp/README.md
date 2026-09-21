# rsi-acp

ACP owns interoperability with operator-selected external agents and IDE clients.
It does not own native Agent durability or process reaping. Stable ACP v1 DTOs
come from the exact pinned schema dependency, with experimental features disabled.
The [protocol library](protocol/README.md) owns external framing and validation.
The [connection driver](core/README.md) owns bounded correlation and output drain.
The [observation journal](journal/README.md) owns separate local external history.
The [client Session owner](client/README.md) binds one peer to that history and
retains prompt, replay and permission lifetimes independently of UI controllers.
Process and Sandbox retain subprocess lifecycle and execution policy.

The product [native adapter](../rsi/acp/README.md) owns translation into Session
operations and controlled-work settlement; it is independent of wire framing.
The [Host plugin](host/README.md) owns configured launch authority, resident slots
and explicit lifecycle over that client service; UI detach never retires a peer.
