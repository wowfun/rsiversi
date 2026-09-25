# rsi-configuration-api

Host pages also carry an optional last completed update with decimal command identity, closed origin, outcome and failure categories. This is not an action receipt or an authorization to retry. Non-Host pages omit it.

The typed configuration-grant API carries redacted authority status and a
Local-only grant snapshot and CAS mutation. Revisions are exact canonical
u64 decimal strings; lists contain at most 64 unique ordered DeviceIds.
Mutation replies must advance the supplied revision once and reflect the
requested membership. Malformed or lost mutation replies remain unknown;
this client never replays a grant change. Endpoint implementations and durable
policy belong to the [configuration owner](../configuration-access/README.md).

`configuration/plugins/3` is a separate grant-gated read. Its explicit target is
Host observations, a current preset preview, or a Session's resident generation.
Preset compilation is pure; Session reads validate the Header correlation and
peek at residency without pinning, preparing or building a generation. A cold
Session reports `not_resident`; it is never presented as the current preset.
Pages contain at most
64 flat instance/plugin identities and closed observed lifecycle states, within
64 KiB; at most 8,192 desired/observed identities are represented. The desired
tree revision and observed Profile-status revision are distinct decimal strings.
No configurations, raw errors, source paths, Runtime graph or dependency keys
are present. Closed reason categories have fixed guidance, and implementation
origins distinguish linked, native and unresolved factories. Preset root classes
are path-free. Pagination must restart if the target, revision pair, availability
or source digest changes. Only Host carries aggregate health and watcher state. Host observations read
Profile lifecycle; Session observations describe the successful activation captured
in the retained composition manifest, not current per-instance lifecycle. The
page target identifies the source, and the GUI labels Session rows as pinned
manifest evidence.
Disabled,
unobserved and active are separate states; configuration alone proves no running
generation. The client validates the echoed offset, complete page progress and
bounded identifiers before exposing a result.

Managed-provider operations carry at most 64 exact provider definitions within
1 MiB. Definition kinds are closed; each concrete provider remains the authority
for its configuration shape. Desired and applied revisions are distinct. Reads
validate bounds and revision order; malformed mutation responses remain unknown.

Credential configuration accepts a closed provider kind and validated owner-local
slot. Version 2 status carries redacted availability, editability, a bounded store
location and closed failure categories. All credential operations negotiate
version 2 together. Determinate storage failures carry the closed
`CredentialStoreFailure` domain payload. After decoding this known failure, the
typed client reports `Backend` with only the category's safe diagnostic; the wire
dispatcher never receives that `Backend` as a mutation result. Writer lock timeout
remains `Capacity`; uncertain writes or malformed replies remain `OutcomeUnknown`.
Set and unset return an
exact operation receipt; there is no remote resolve, implicit provider apply or
default selection. Secret serialization is restricted to the immediate set request.

`providers/discover/1` takes a closed provider kind, endpoint and owner-local
credential slot before a deployment exists. It returns at most 4,096 candidate
model identifiers with optional display names and token capacities, within
4 MiB. Discovery requires configuration authority and performs provider I/O;
it does not register routes or persist configuration. The ordinary Models API
continues to enumerate only configured routes without provider I/O.
Its `Read` effect selects cancellable work ownership, as defined by the API
protocol; it does not confer configuration authority or imply absence of HTTP I/O.
Discovery byte limits are enforced by the upstream HTTP body reader and the API
reply encoder/transport. Provider parsers validate candidate semantics; the client
checks the echoed request identity and candidate semantics after bounded decoding.
Typed intermediate snapshots are neither revalidated nor serialized merely to
measure them. Field/count bounds alone do not bound JSON-escaped reply size.

MCP configuration uses separate finite `mcp-configuration.* / 1` operations.
Status, explicit refresh and credential setup each require a held configuration
grant in their Host handlers. Status contains closed readiness/error categories,
transport kind, epochs, last verified digests and bounded Tool name choices, without
URLs, commands, environment values or server instructions. A remote caller can
refresh HTTP endpoints; refreshing a stdio endpoint requires Local origin.
Credential actions bind both server identity and the exact current owner-local
reference. Secret writes have independent receipts and are never replayed.
Exa credential operations are a fixed owner-scoped status/set/unset contract.
Every Host handler retains the actual Configuration grant through completion.
Receipts contain no secret or local store path, and never enable Tools or submit
a search. The fixed `rsi.retrieval/exa` binding is not an AI provider slot.

The Plugins wire contract is v3. It includes an explicit target, asynchronous
resident-generation lookup, health/watcher evidence and the typed last update
attempt. Earlier versions are unsupported; in-tree clients use v3 together.

Host Profile leaf management uses separate `profile-leaves` version 1 operations.
Its exact root identity, user Profile, leaf and enable/disable/configuration
operation require an explicit Local-issued scope grant, including for a Local
caller. Device callers also retain their existing configuration admission;
Agent tools have a distinct Session principal and cannot inherit human authority.
Local grant changes are unavailable through remote authentication.

The source owner returns bounded redacted catalog metadata and prepared previews.
A preview binds its proposal, original source, dependencies and frozen catalog to
one Host epoch and ticket. It returns no configuration, source text or diagnostic
payload from plugin code. Each caller may list and discard its own unused previews,
including after losing a preparation reply. Commits retain
one request identity and can be reconciled after reply loss; clients never retry
an unknown write as a fresh operation. A caller may recover its at most 256 receipt
ticket identities after reconnecting, then query the exact receipt. Source publication, directory durability
and observed application are separate fields. A replaced Host reports old
runtime tickets unavailable, never silently submits their writes again.
Commit replies must echo the complete redacted preview. Malformed or conflicting
mutation metadata remains unknown even when the JSON itself decoded successfully.

Requests are at most 128 KiB, configuration 64 KiB / depth 32 / 4,096 values,
catalog pages 64 leaves, responses 64 KiB, and grants 256 exact scopes. The owner
retains at most four previews and 256 receipts, with two nonqueued read/prepare
slots, four retained grant-change slots, and one source/grant writer. Admitted preparation and writes survive response
loss; revocation closes scope admission before waiting for already admitted work.
The revoked grant is published before that drain; unrelated mutations can proceed
while old admitted work settles. Revocation acknowledgement still waits for the
drain, and it never overwrites a subsequently issued grant revision.
