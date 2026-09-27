# Applications and Sessions

Native Session artifacts use the shared [Session export contract](../../session-export/README.md).
The [terminal](../../../../apps/terminal/README.md) and [GUI](../../gui/README.md) entrypoints authorize
the selected handle and deliver the same fixed-cut Markdown or JSON stream.

On Linux, `serve` composes a publishing Service Host and the independent
[HTTP application](../../../../apps/serve/README.md) in one Runtime. Configure the standard Host's
explicit `rsi.agent.default_model` setting and provider Profile before serving
coding Sessions. For an isolated development listener use
`rsi --profile serve --bind 127.0.0.1:8787 --origin http://127.0.0.1:8787 --dev-http`.
Production instead supplies an HTTPS origin, `--tls-certificate FILE` and
`--tls-key FILE`. HTTP always requires device authentication; the same process
also publishes the existing same-user local endpoint. `host status`, `reload`
and `stop` address that owner normally. `host serve` remains the foreground
local-daemon management entry. Compose the independent [Web assets plugin](../../web-assets/README.md)
with the HTTP application to serve the [Web application](../../../../apps/web-worker/README.md).
The [Web build and launch reference](../../../../apps/web/README.md) provides
the application Profile and bundle command. Its Rust Worker owns shared client
controllers and two independent panes; the document renders views and forwards input.

`rsi --profile inspector runtime [AFTER_FIBER]`, `profile [OFFSET]`,
`factories [OFFSET]` and `native` read one finite local operator JSON page from
an existing Linux Service Host. The response includes its next cursor and total;
Inspector does not create a Session or start a missing service. It shows actual
runtime ownership, redacted Profile nodes, frozen factory provenance, and native
selection/retention. [Inspector](../../inspector/README.md) owns its wire and paging
contract; authoring remains under the explicit Profile management commands.

`rsi --profile devices register LABEL` returns one JSON receipt with EndpointId,
device id, label and its one-time token. `list` returns non-secret records;
`revoke DEVICE_ID` revokes that exact device. These commands use the live owner's
same-user local API and never edit its credential database independently. Keep
the registration receipt for browser login or explicit remote client setup.
An unknown registration outcome requires list/revoke reconciliation; the CLI
does not silently issue another credential. Ordinary remote credentials cannot
invoke device administration.

An explicit native remote Application Profile composes `rsi.credentials.local`
and `rsi.application.http`, followed by its CLI, Headless or TUI plugin. The HTTP
connection configuration is the [native HTTP client contract](../../../rsi-api/http-client/README.md):
explicit origin, expected EndpointId, credential reference and optional CA file.
It negotiates wire and domain operations without the local executable hash gate.
Credentials configuration holds references and environment names only; the
launcher captures `RSI_API_DEVICE_TOKEN` alongside the standard provider variables.
The connection plugin prepares transport policy before activation, composes HTTP
and independent domain clients in a child Profile of the same Runtime, and
publishes their capabilities with remote-detach lifetime. It never creates local
backend state or signals the remote service on exit.

The Session service creates drafts from registered Workspace identities, attaches,
and lists sessions, then exposes one
handle for ordered text-and-image mailbox submission, direct Image generation,
cancellation, reconnectable observation, bounded backward history, and live
approvals. Callers allocate a `MessageId` for Language or multimodal input.
Acceptance atomically persists the immutable Header when needed and a canonical
mailbox record, but it does not invent a `TurnId`; the later durable claim
creates the Turn and first Step. Retrying the same identity and body returns the
indexed message state across reconnect or restart, while a changed body is a
typed conflict. Agent-control records and Facts are independent durable streams.
Approval waiters remain bounded live Host state and are never replayed as
effects.

The Rust Session interface additionally exposes direct Image generation. Its
caller allocates the `TurnId`; the operation validates its exact Image route and
does not require the session's default Language deployment to be available.
Image results remain Media references.

Local browser startup follows the [Local Web application](../../README.md#local-web-application)
entry point and the [Serve contract](../../../../apps/serve/README.md).