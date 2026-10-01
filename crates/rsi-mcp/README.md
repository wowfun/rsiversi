# rsi-mcp

MCP is an opt-in integration above Agent composition, Tools, Credentials, Media
and Process. It owns remote protocol negotiation and finite discovery; it does not
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

SSH stdio is a separate explicit transport. It names one target, a target command,
target cwd and explicit extra environment. Target account defaults and fixed PATH
come from Execution; no Local stdio inputs or Service environment are copied.
Configuration and refresh require the product's exact target/server management
grant and its credential-reference allowlist before any credential resolution.
Ordinary HTTP configuration never grants remote process launch. Remote stdio
startup remains unavailable until an explicitly admitted refresh; cold restoration
does not recreate grants or connect implicitly.

A verified SSH server retains its original Execution duplex resource. Every new
business RPC requires the current caller's lease from that exact provider and a
fresh Use admission after the bounded RPC queue. Accepted exchanges retain their
permit until response or controlled retirement; another caller cannot borrow the
configuration author's grant. Reconnect requires an explicit refresh, even if the
path and target name are unchanged. Session Tools and resource reads pass their
original caller lease; a missing or foreign lease performs zero target I/O.
Fixed replies to server ping/unsupported requests are accepted connection
maintenance, with one active and one queued reply. They carry no caller inputs or
credentials and use a separate bounded reply operation. This keeps idle protocol
maintenance independent of a creator's revoked grant without granting business RPC.
Stdio drains stdout independently of serialized stdin writes. Server requests
use one active and one queued bounded reply; overflow closes the connection.
This prevents a server reply from blocking the only stdout reader behind an
in-flight client frame. Both pumps stop with the connection epoch.
Server instructions are attributed external data,
exposed through explicit resource reads rather than system instructions.

## Resource templates

Each server must explicitly enable `resource_templates`; it defaults to false.
Enabling it authorizes that server's template-defined resource range, including
reserved expansions. It grants no local Files authority. Disabled discovery sends
no template RPC. MethodNotFound records Unsupported; all other discovery errors
reject the complete candidate catalog. A malformed enabled template also rejects
the candidate, preserving the last verified catalog.

Manifest codec 3 records complete template metadata and its discovery state.
Resources, templates and instructions share the 256-entry and 256 KiB manifest
bounds. Template metadata participates in the manifest digest; the opt-in belongs
to the configuration fingerprint. A template-only server still registers the
shared resource reader. Saved older codecs are rejected explicitly.

`mcp_resource_read` accepts `server`, optional opaque `id`, and optional
`parameters` only for `template:N`. Static IDs reject parameters. An absent ID
lists the frozen catalog. Parameters admit strings, string lists and string maps:
at most 32 variables, 256-byte variable names, 256 scalar leaves, 4096 bytes per
string and 64 KiB encoded total. Unknown/invalid names are rejected; absent
variables follow RFC 6570. URI expansion uses `iri-string` 0.7.14 with UriSpec
and a 4096-byte output writer. Expanded URIs remain server resource identities,
never URLs fetched by this client. The text-only Session resource interface
retains its existing contract.

MCP Tool images and image resource blobs use canonical base64 decoding and Media
source-MIME validation. All images share a 32 MiB canonical output budget per
result; each import receives the remaining encoding allowance before publication.
An import response exceeding that allowance rejects the image projection without
underflowing the shared budget or publishing a partial image batch.
Raw JSON remains unchanged in the structured result, including `isError` and
server metadata. Image import does not depend on a model profile. The
[Context owner](../rsi-agent/context/README.md) chooses the model-visible projection
for each request. A failed image batch emits diagnostics in all image positions,
retains raw JSON and does not replay the RPC. Earlier successful immutable imports
may remain; there is no batch rollback or garbage collector. The 1 MiB frame
limit remains in force, so many large screenshots cannot enter this path.

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

Stdio retirement propagates Process settlement errors. The service latches any
failed settlement, including replaced or cancelled connections, and reports it
after draining all endpoints at shutdown; plugin disposal cannot claim a clean
shutdown after a reaping or pipe-settlement failure.

Model-facing Tool and resource exchanges mark dispatch at HTTP send or the first
stdio write, after queue, frame and credential preflight. Cancellation before
dispatch remains a rejection. Once dispatched, an unverified reply, disconnect
or deadline is `OutcomeUnknown`, propagated as a typed Tool failure. A verified
JSON-RPC error or explicit protocol rejection remains a known result. The owner
still drains the retired connection; cleanup failure cannot erase uncertainty.
Discovery retains its existing closed diagnostics and last-good catalog policy.

An uncertain stdio process start also preserves `OutcomeUnknown` before the first
MCP write. The process may already have run initialization effects; connection
setup cannot convert lost spawn acknowledgement into `ProcessUnavailable` or
retry a protocol-era probe by starting another process.
