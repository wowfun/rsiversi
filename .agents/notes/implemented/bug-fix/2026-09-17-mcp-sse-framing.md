---
name: Chunk-independent MCP SSE framing and response settlement
---

## Problem

Network chunk boundaries do not delimit SSE events. A decoder that assumes LF,
rejects a notification burst according to chunk size, or waits for EOF after a
correlated response can reject legal streams or hold an exchange indefinitely.

## Decision

The [MCP transport contract](../../../../crates/rsi-mcp/core/README.md) owns framing
and limits. Its incremental parser accepts LF, CR, split CRLF, one initial UTF-8
BOM, and SSE fields across arbitrary transport cuts. Per the
[HTML SSE field grammar](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation),
a colonless `data` line contributes an empty data value; the assembled payload
must still be valid JSON-RPC. Bounds apply before JSON materialization.

Parsing yields after 16 KiB or 64 messages, independently of chunk partitioning.
Yielding preserves both framing state and the exchange's byte admission budget.
A normal request settles at its first correlated response; subsequent bytes,
duplicate responses, and mismatched trailing responses are outside that exchange.
The separate notification watch owns its stream lifetime.

## Alternatives considered

Requiring LF or `data:` would narrow legal SSE framing. Rejecting an entire large
network chunk would tie admission to transport batching. Parsing trailing events
after settlement could turn a valid result into an unrelated protocol failure.
Yielding only between chunks would not bound monopolization by a large chunk.

## Consequences

Partition tests cover line endings, BOM, UTF-8, empty fields, bursts, and yields.
Legacy and modern HTTP fixtures cover coalesced duplicate/mismatched/invalid tails
and a server that stays open after responding. Limits remain fail-closed before
settlement. This does not add same-server call concurrency or mutation replay.

Named control events also require dispatch filtering. A bounded `event: ping`
block containing non-JSON data must not terminate the JSON-RPC connection. The
last event name in a block selects dispatch; absent, empty and `message` names
are accepted, and the name resets even on empty-data blocks. This matches the
installed official Rust SDK source (`rmcp` 1.7.0,
`transport/common/client_side_sse.rs`, message-event classification). Split-boundary
tests include `id` and `retry`; neither enables reconnection or replay here.
