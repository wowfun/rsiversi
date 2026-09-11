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
slot. Status contains only availability and editability. Set and unset return an
exact operation receipt; there is no remote resolve, implicit provider apply or
default selection. Secret serialization is restricted to the immediate set request.
