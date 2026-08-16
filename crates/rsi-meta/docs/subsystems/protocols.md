# Control and service protocols

The CLI-owned v0 control protocol projects `CompositionHost` onto Unix line-delimited JSON and WebSocket text messages. Exact envelope fields belong to the [published schemas](../../../../schemas/rsi-meta/). The embedded core exposes typed methods rather than control envelopes.

## Commands, identities, and results

The command vocabulary is `apply_manifest_path`, `query_graph`, `query_events`, `inspect_plugin`, `rotate_token`, and `shutdown`. `validate` and `lock` are offline CLI operations, not daemon commands. Unknown command types receive a structured `unsupported_command` result.

For `query_graph`, `query_events`, and `inspect_plugin`, `command_id` is connection-local correlation. It may be reused after the prior request completes and is never persisted; a concurrent duplicate on one connection is rejected. For apply, token rotation, and shutdown, it becomes a durable `OperationId`: an equal retry returns the stored terminal result and different parameters return `operation_id_conflict`. Command and operation IDs contain 1-255 printable ASCII bytes without spaces; host-reserved operation prefixes remain unavailable to callers. `expected_graph_revision` is legal only for apply.

Apply results are `applied`, `no_change`, or `restart_required`. `restart_required` leaves the daemon available and does not imply restart or shutdown. Rejections have stable `code`, human-readable `message`, and optional structured `details`. For `query_events`, `after_cursor` defaults to zero and `limit` defaults to 1,000; the store clamps it to 1 through 10,000.

## Durable events and resumption

Every event receives a monotonic cursor before broadcast and carries the graph revision plus an optional `operation_id`. System lifecycle events omit the operation ID. `composition_committed` is emitted only when routing becomes active; `host_shutting_down` records explicit shutdown. There is no `daemon_restarting` event.

A `query_graph` result contains a graph and cursor from the same immutable routing snapshot. `/ws?after=N` first replays events with cursor greater than `N`, then continues live without a gap. Generic clients retain unknown event payloads.

Close reasons begin with stable space-separated `key=value` fields:

| Status | Reason prefix | Client action |
|---|---|---|
| 1008 | `code=bearer_token_rotated` | Reload the token and reconnect. |
| 1013 | `code=event_stream_interrupted last_cursor=N` | Reconnect to `/ws?after=N`. |
| 1013 | `code=event_delivery_interrupted last_cursor=N` | Reconnect after `last_cursor`; the cursor advances only after send. |
| 1012 | `code=daemon_restarting` | Reconnect after the supervisor starts the reconciled daemon. |

Other terminal reasons are `code=daemon_shutdown`, `code=event_subscribe_failed`, `code=message_too_large`, `code=outgoing_message_too_large`, `code=outgoing_delivery_failed`, `code=invalid_envelope`, and `code=text_required`. Authentication or Origin rejection occurs during upgrade and has no close frame.

## Service streams

An `open` identifies a consumer instance and contract. Provider selection comes from the committed graph, and an explicit binding is the only cross-branch route. The host pins exactly one provider generation for the stream lifetime. Successful open acknowledges the stream with `{"provider": "provider-instance-id"}`.

Client operations are `open`, `data`, `credit`, `half_close`, and `cancel`; provider events are `data`, `credit`, `end`, and `cancel`. Sequence numbers begin at 1 independently in each direction. DATA consumes credit equal to the UTF-8 byte length of its encoded payload array. `half_close` closes one sender, `cancel` closes both directions, and exactly one `end` or `cancel` is terminal. Streams are connection-scoped and never replayed.

Connection ingress, control egress, plugin control/data lanes, frames, and outstanding credit are independently bounded. Each listener admits at most 128 connections, HTTP peers have five seconds to finish headers, and each HTTP connection has a 30-second maximum lifetime so an idle keep-alive peer cannot retain admission indefinitely. A lagging event subscriber disconnects with its last durable cursor; service DATA is never persisted.
