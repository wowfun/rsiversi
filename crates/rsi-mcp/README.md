# rsi-mcp

MCP is an opt-in integration above Agent composition, Tools, Credentials and
Process. It owns remote protocol negotiation and finite discovery; it does not
own Agent state transitions or grant Tool execution permission. The protocol
package owns typed configuration and complete frozen manifests. The ordinary
core plugin owns connection epochs, verification, transport work and retirement.

New Agent generations capture the current verified manifest. Each Session retains
its complete typed manifest in one Agent Domain before execution. Restoring that
Domain reconstructs definitions through the generic pre-seal seed without live
server discovery. A changed or disconnected server returns an explicit Tool result
error; it never silently changes a Session's registered schema. ToolPolicy remains
the single authorization owner; MCP annotations are descriptive only.

Streamable HTTP uses explicit endpoints and optional CredentialRef bearer tokens.
Credentialed endpoints require HTTPS except loopback IP literals and the exact
`localhost` name, which the transport pins to loopback addresses. Redirects and
ambient proxies are disabled. MCP endpoints are operator-selected authorities,
including private services; they do not use Retrieval's public-web DNS policy.
Stdio uses the sibling duplex Process contract, an absolute command/cwd, explicit
argv and environment, and a Sandbox-produced plan. It never resolves an executable
from ambient PATH. Only Local configuration may change stdio commands or launch
parameters; remote configuration may read their redacted status. These services
are independent of Session sandboxes: a Local stdio entry explicitly authorizes
an unconfined Host service with its configured executable, cwd, argv and environment.
Sandbox produces and records that plan; this does not claim process confinement.
Only configure trusted local server programs. Started RPC calls
are not automatically replayed. External call results cross the complete Tool result
validator before publication. Unsafe text, excessive JSON depth/nodes or result
size produce a bounded Capacity Tool error without retaining the rejected payload.
Stdio drains stdout independently of serialized stdin writes. Server requests
use one active and one queued bounded reply; overflow closes the connection.
This prevents a server reply from blocking the only stdout reader behind an
in-flight client frame. Both pumps stop with the connection epoch.
Server instructions are attributed external data,
exposed through explicit resource reads rather than system instructions.

HTTP settings and Local stdio configuration have distinct visibility. The Host
Profile owns stdio launch inputs; the remotely readable `rsi.mcp` Settings namespace
accepts HTTP entries only. A saved HTTP change requires explicit apply/refresh and
marks fresh composition input unavailable until applied and verified. Startup makes
one bounded attempt. Remote configuration grants permit HTTP refresh and independent
credential setup; a stdio refresh requires Local origin.

The asynchronous retirement wait has a 30-second deadline. If old connection retirement
outlasts that wait, the caller receives Timeout after replacement has applied;
the tracked retirement retains configuration admission until its real work ends.
Status remains readable and started calls are never retried by this wait.
Status retains the last verified catalog and reports current readiness separately.
Manifest admission counts selected Tools plus the optional `mcp_resource_read`
against the shared registrar ceiling. Other contributions are accounted for by
the actual selected preset registrar; MCP cannot reserve a guessed budget for them. A verified MCP manifest
still has to fit the actual Session Tool Runtime alongside every other contribution.

## Protocol revisions

The client prefers [MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/changelog).
It probes `server/discover` before any business RPC. Modern requests carry the
version, empty client capabilities and client identity in `params._meta`; HTTP
also carries matching protocol, method, name and schema-designated parameter
headers. Modern epochs neither initialize a session nor use HTTP GET/DELETE or
session IDs. Catalog subscriptions use `subscriptions/listen` with an acknowledged,
exact request ID and explicit Tool/resource list-change filters. A lost subscription
invalidates the epoch; no started Tool call is replayed.

Legacy 2025-11-25, 2025-06-18 and 2025-03-26 servers retain their specified handshake
and transport semantics. Only the side-effect-free discovery probe permits era
fallback: non-modern RPC errors, HTTP 400 without a recognized modern error, or a
bounded silent stdio probe. A silent probe is reaped before one legacy reconnect,
so a late reply cannot satisfy another request. Recognized modern errors never
trigger a downgrade. Unknown protocol versions fail explicitly.

Modern results require `resultType`; legacy omission means `complete`. Unknown
result types fail closed. `input_required` is an explicit unfinished operation,
never a successful Tool result or an automatic retry. The client advertises no
sampling, roots, elicitation, Apps or Tasks capability. It never gathers or returns
these inputs implicitly. Required cache fields are admitted as bounded metadata;
TTL hints do not replace the Session's frozen manifest or share results between
credentials. Remote JSON Schema references are not fetched. Tool schemas remain
bounded and preserve composition keywords and structured output without coercion.

HTTP parameter annotations use only `Mcp-Param-*` headers. Invalid annotations
reject complete discovery, preserving this owner's no-partial-catalog contract;
this is stricter than the specification's recommended per-Tool omission. Values
use the standard Base64 sentinel when plain ASCII would change their meaning or
be unsafe. Missing values omit their headers; integer values obey the specified
safe range. Header or capability failures return explicit errors without replay.
Operator-supplied bearer credentials remain the supported authentication path;
this client does not advertise an OAuth authorization or registration flow.

## Verification

Run `cargo test -p rsi-mcp-protocol -p rsi-mcp` for finite wire, discovery, credential,
retirement and actual duplex-process behavior. Tests use isolated loopback servers
and, on Unix, an explicit Python fixture under the managed Process provider.
`cargo test -p rsi --test mcp` exercises standard Host composition, frozen definitions,
exact seed reconstruction after restart, aggregate Tool registration and real grants.
Browser product fixtures exercise the same granted workbench operations. External
MCP servers and model providers are opt-in integration evidence, separate from these
local deterministic tests.

Fresh seed capture checks current connection readiness on every call, then reuses
the immutable encoded seed while all frozen server identities are unchanged.
Repeated pins do not clone schemas or re-encode the complete manifest.
