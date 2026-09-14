# rsi-configuration-api

The typed configuration-grant API carries redacted authority status and a
Local-only grant snapshot and CAS mutation. Revisions are exact canonical
u64 decimal strings; lists contain at most 64 unique ordered DeviceIds.
Mutation replies must advance the supplied revision once and reflect the
requested membership. Malformed or lost mutation replies remain unknown;
this client never replays a grant change. Endpoint implementations and durable
policy belong to the [configuration owner](../configuration-access/README.md).

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
