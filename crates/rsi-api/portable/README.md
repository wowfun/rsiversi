# rsi-api-portable

This adapter transports an explicitly supplied `ApiClient` capability through
ordinary Meta Portable calls. It never decodes a caller origin, Context or Local
contract key from a request, never imports product domains, and never creates a
Runtime. An exporter fixes an explicit set of complete `OperationKey` grants.
Creation is synchronous; empty grants and temporarily absent operations are valid.
Discovery intersects the current upstream directory with that fixed set. Each
business call checks its grant, resolves the current single-key upstream policy,
checks call shape and applies export and upstream byte bounds before forwarding.
Policy resolution occupies a Control slot before making any upstream request;
the registered execution-class slot then owns payload receipt and forwarding.
The final upstream dispatcher independently admits against its current registration.
Foundation operations are handled locally, without recursively querying upstream.
The export owns description, full-directory and required single-key discovery;
single-key discovery uses Control and full-directory discovery uses Data.
Neither discovery nor later registration can enlarge the grant. Duplicate grants
are rejected. Profile `ExportConfig.operations` contains keys, not bare IDs.
Both export entry points validate complete grant keys before publishing capability.

An importer requires `rsi.api.portable` version 2, captures connection identity
and publishes the ordinary Local `ApiClient` contract with live discovery. The
captured Portable capability fences both caller and provider generations. An
exporter may publish its captured capability locally for deliberate transfer to
another Portable call. Such transfer grants exactly the exported API authority;
semantic target narrowing belongs to the product that supplies the client.
Cloning a Local value does not change its Meta holder. A transfer must originate
in that holder's exact generation; an independently injected API capability and
destination capability can be sent together when both belong to the same caller.
An owning product plugin can call `export_api` during activation to register its
already narrowed client and receive a capability in that same generation. The
helper installs ordinary deferred cleanup and early retirement observation before
returning the grant; it does not construct another plugin graph or Runtime.

The wire has closed, bounded headers and separate binary fragments. JSON data,
including domain errors, is never base64 encoded or copied into another JSON
field. Fragments carry exact contiguous offsets and at most 64 KiB of payload.
Request reception reserves the operation's input storage before allocation;
response reception reserves the complete declared JSON plus binary size before
reading either part. Input, output and frame scratch have separate budgets.
Each export admits at most 16 Control, 16 Data and 64 Subscription calls.
Description negotiation shares the Control lane, retaining its slot through
response transmission, including while the peer has not finished its request.
Escaped bytes retain the same response pool; provider output remains retained
until transmission completes. Empty, oversized, gapped and unexpected packets
cannot become a successful reply.

A finite result is published only after the expected payload and clean Meta
terminal result. Subscription items are complete bounded API messages followed
by an explicit stream end and clean terminal result. Cancellation drops reads
and streams. Once a complete mutation is dispatched, the exporter's ordinary
owned task continues even when the Portable response waiter disappears. Export
retirement fences admission, cancels reads and joins those tasks before releasing
the supplied client. Neither side automatically replays a mutation. Uncertain
transport completion maps to OutcomeUnknown for mutations; domain receipts remain
with their owning APIs.

The importing plugin observes its exact Meta generation's admission closure and
retires the shared client before deferred cleanup. This releases an outbound
call even when its consumer leaves a full subscription queue unpolled. Deferred
cleanup then joins that client; it cannot be the first notification because Meta
drains outbound admission before running effects.

Portable calls inherit the owning Runtime's existing service-call deadline
(default one minute). The adapter does not silently extend it or hide reconnects.
Read-stream consumers use their domain's explicit cursor/reconnect behavior;
new UI observations create new presentation epochs and tickets. This is the
existing Meta channel contract, independent of HTTP subscription lifetime.

Run the adapter tests through real Meta channels, including cancellation,
fragmentation, malformed terminal results and retained-byte admission. Native
SDK and product/browser integration are separate evidence.
