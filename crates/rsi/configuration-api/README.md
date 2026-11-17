# rsi-configuration-api

Interface version 6 uses stateless Host Profile previews and direct commit results.

`agent-presets` version 1 exposes read-only `list` and `document` operations under
the caller's retained configuration grant. Each read captures the current Agent
composition source, using its existing preset catalog and compiler; it neither
activates a generation nor creates a second catalog. Filesystem discovery and
document compilation run outside the async executor. Lists preserve catalog
order, current default and broken-preset diagnostics. Health means pure Profile
compilation succeeded, not that activation has been tested.

Documents return exact native TOML with an explicit `toml` format, never a
purported DSH plugin program. Missing and broken documents have distinct domain
errors. Requests are at most 1 KiB; encoded lists are at most 16 MiB and documents
4 MiB. The catalog owns filesystem and metadata limits; the API encoder owns the
wire-size limit and returns capacity exhaustion instead of truncating content.
Default changes continue through the existing `rsi.agent-presets` Settings
namespace. Session selection continues through the revision-checked Draft
command; these reads confer neither mutation nor Session authority.

Host pages also carry an optional last completed update with decimal command identity, closed origin, outcome and failure categories. This is not an action receipt or an authorization to retry. Non-Host pages omit it.
`partial` means the ordinary reload accepted its desired target and retained
successful leaves while reporting a primary failure without compensation.
`rolled_back` and `degraded` retain their executable-input restoration meanings.

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
the actual ingress caller's execution-location access in addition to the
configuration grant. The source receives that origin without Local substitution.
Session reads peek at residency without pinning, preparing or building a generation. A cold
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
default selection. Secret serialization is restricted to immediate set or explicit
transient discovery requests; it never enters a response or retained setup view.

`providers/discover/2` takes a closed request identity (provider kind, endpoint and
owner-local credential slot) and an optional transient key before a deployment
exists. A supplied key is used only for this read; otherwise the slot is resolved.
The reply echoes only the non-secret identity. It returns at most 4,096 candidate
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
The `ssh-get`, `ssh-put`, `ssh-remove` and `ssh-refresh` operations use exact
target/server stdio grants, independently of ordinary configuration permission.
One grant must include every referenced credential; permissions from separate
grants are never combined. Refresh also obtains the caller's target Use lease.
Checks precede credential resolution and remote process work. Local stdio entries
cannot be changed through these operations.

The product persists the complete bounded SSH stdio document in its own Storage
Domain (eight servers, 256 KiB). Its monotonic document revision fences every
mutation and refresh, including deletion and recreation. Reads return only the
selected authorized server and the document revision. A committed mutation may
report a connection-apply error separately; it never pretends to roll back durable
configuration. Cold startup restores configuration without reconnecting; explicit
refresh supplies current authority. The MCP owner merges HTTP, Local stdio and SSH
stdio identities before applying them, rejecting collisions and aggregate limits.
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

Host Profile leaf management uses separate `profile-leaves` version 3 operations.
Its exact root identity, user Profile, leaf and enable/disable/configuration
operation require an explicit Local-issued scope grant for human clients, including
a Local caller. Device callers also retain their existing configuration admission.
Native Agent tools use actual Turn authorization: Full access or per-call approval,
with independent `require_approval` preserved. Their distinct Session principal authenticates
management calls; Agent principals cannot be stored in the grant document.
The Tool reports a source owner's explicit ownership refusal as Unauthorized,
not as an unknown write outcome. Unavailable outcomes retain reconciliation guidance.
Local grant changes are unavailable through remote authentication.

The same grant document owns a closed scope union: Profile leaf change, exact SSH
target use, exact SSH target management, or exact target/server stdio management.
The latter binds a sorted unique list of at most 32 `rsi.mcp` credential references.
No scope implies another scope, and ordinary configuration grants imply none of
these SSH scopes. Local host-key and authentication-identity trust remains a
separate Local-only operation. Grant storage uses domain version 4 with a typed global and retains
the existing CAS, gate-close, Storage-fence and admitted-operation drain contract.
Earlier grant stores, including stored Agent grants, are unsupported. They must
be explicitly reset or replaced with fresh state before startup; boot does not
silently discard grants or migrate their authority.

The source owner returns bounded redacted catalog metadata and stateless previews.
A preview binds original source, dependencies, the proposed change and the frozen
catalog to one Host epoch and review digest. It retains no source, Host, permit or
server ticket after the call. Commit supplies the target, literal change and review
digest again; the owner rechecks authority and prepares the exact change under its
serialized writer before publication. Changed inputs return Conflict.

The completed result is returned directly and is not retained. There is no receipt
lookup, preview inventory or discard operation. Source publication, directory
durability and observed runtime application remain separate fields. A lost response
is unknown; clients refresh current desired/applied state before choosing further
work and must not describe that observation as proof of the original operation.
Commit replies echo the redacted preview. Malformed mutation replies remain unknown.

Requests are at most 128 KiB, configuration 64 KiB / depth 32 / 4,096 values,
catalog pages 64 leaves, responses 64 KiB, and grants 256 exact scopes. Local,
Device and Agent each have two short-lived operation slots; writer waiters hold
those slots. Grant changes have four separate slots. Source/grant publication is
serialized. There is no per-principal fairness guarantee. Admitted writes survive
response loss; revocation closes authority before publication and drains work
already admitted under the revoked grant. Preview completion retains no capacity.

## SSH targets

The `ssh-targets` version 1 operations provide a bounded catalog, candidate CAS,
Local-only trust confirmation, connection, disconnection and target directory resolution. Candidates contain
only an identity, display name and strict literal endpoint. Expected revisions
are canonical decimal strings; zero means creation and is never a stored revision.
Responses reveal a pinned host-key fingerprint, current connection epoch and
missing program selectors, never identity paths, private bytes or credential values.
Every mutation names the negotiated Host epoch. Catalogs contain at most 64 targets;
request and response limits are 32 KiB and 128 KiB. Unknown fields are rejected.
The owner uses the existing leaf grant union's exact SSH Use and Manage scopes.
An authenticated device's ordinary configuration grant permits candidate submission,
not trust confirmation or connection. Local trust explicitly supplies and confirms
the public-key fingerprint and selects a bounded absolute identity path.

Directory resolution requires current Use and an exact target revision. The pinned
Execution lease performs canonicalization on the target; returned coordinates are
validated data and do not authorize subsequent registration or file access.
Connection mutations also compare the last observed connection epoch, so an old
view cannot disconnect a replacement connection.
