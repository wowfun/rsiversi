# rsi-mcp

A private protocol owner may explicitly construct a service with
`new_with_all_discovered_tools`. This constructor policy selects every discovered
Tool in that service's frozen manifest, within the same aggregate limits. It
does not change the normal configured-name policy, modify Settings or grant
authority to any other service. Selection remains frozen in the manifest and
must be compared again before restoring a saved Session.

`McpToolsFactory::with_service` binds a prepared private MCP service explicitly
instead of looking up the standing Host service. Its owner prepares discovery
and supplies the matching frozen manifest before Agent catalog publication,
retains that exact service through admitted work, and shuts it down after
settlement. The ordinary global factory and private factory use the same Tool,
Domain and contribution registration path; private providers never modify the
Host's configured endpoints or inherit its credentials implicitly.

The ordinary MCP owner retains connection work through cancellation and retirement.
Fresh composition obtains a verified manifest snapshot; offline restoration receives
its saved Domain seed. HTTP and managed stdio implement the same finite RPC owner,
which validates frames and response identities before publishing results. Its
registry exposes no permission shortcut around normal ToolPolicy admission.
Each frozen server catalog carries its immutable digest and encoded size.
Observation checks connection health and aggregate catalog limits using these
proofs; it does not clone schemas or assemble a composition seed. Tool/resource
dispatch compares frozen digests, retaining exact schema and target fencing.
Frozen catalogs enter aggregate validation in ascending server-ID order. The
protocol owns the shared catalog rules; the live owner additionally checks the
complete envelope size from immutable server lengths. Tests compare that count
and acceptance with actual manifest encoding at the Domain byte boundary.
HTTP requests disable automatic decompression even under Cargo feature unification;
encoded responses are rejected before frame parsing. Server-request replies on the
idle event stream have the same 30-second operation bound as ordinary exchanges.

SSE framing is independent of transport chunks. It accepts LF, CR and CRLF,
including split CRLF and an initial split UTF-8 BOM. Lines and accumulated event
data retain their frame limits. Incremental consumption yields after 16 KiB or
64 messages without rejecting a larger transport batch. Incomplete events at EOF
are not dispatched. Ordinary HTTP RPCs finish on the first complete correlated
response; preceding notifications are processed in order, and trailing events
are outside that exchange. Subscriptions retain their separate stream lifetime.
HTTP request bodies are bounded and encoded once before dispatch.
Business requests retain their exact connection epoch through preparation and
waiting. At most nine requests may be outstanding per connection, including
preparation and the single active exchange; further requests fail with Busy.
Prepared requests wait fairly at the exchange gate with at most 1 MiB of encoded
payload each. The 30-second business deadline includes preparation, waiting and
exchange, and is never renewed on dispatch. Caller cancellation or timeout before
exchange only releases admission. Once exchange starts, uncertainty retires the
epoch; queued work fails with that epoch and never migrates or replays. Discovery
retains its sequential negotiation and separate probe deadline.
An uncertain started request awaits the retired connection's shutdown before
returning, including managed stdio child reaping and pipe settlement. Cancellation
before exchange admission leaves a healthy connection available. This local
settlement does not establish completion of work on a remote HTTP peer.
Resource listing constructs the full frozen descriptor list; exact reads construct
only the selected descriptor and require canonical opaque IDs. Empty registered
resource sources remain visible through the Agent Sources operation.
As in the SSE field grammar, bare `data` means an empty data field. An event
containing only empty data still fails the JSON-RPC payload check; empty data
lines surrounding a valid JSON value contribute ordinary JSON whitespace.

The Host owner registers HTTP endpoints in the `rsi.mcp` Settings namespace.
Local stdio entries are supplied by the owning plugin's Local Profile configuration;
they are not exported through the remotely readable Settings document. Endpoint
credentials remain owner-local Credentials references. Saving HTTP settings marks
new composition input unavailable until an explicit connection refresh applies and
verifies the saved target. Startup performs one bounded refresh attempt. A refresh
never replays a previously started Tool call. The current saved HTTP configuration
and redacted actual endpoint observations have separate meanings in the workbench.

The ordinary SSE exchange byte budget applies to input admitted through its first
correlated response. A transport chunk may include a trailing suffix beyond
that point; the suffix cannot turn an already complete response into Capacity.
An explicitly oversized Content-Length is still rejected before body parsing.

Only absent, empty, or `message` event names dispatch JSON-RPC. Other named
SSE events are ignored after bounded framing, including non-JSON keep-alives.
`id` and `retry` fields are ignored: this transport does not implement SSE
reconnection or Last-Event-ID replay. Event names reset at every blank line.

Failed process settlement remains a service shutdown error. Configuration reports
settlement failure for entries retired by that operation; historical failure in a
different entry does not reject later unrelated configuration changes.
