# rsi-execution-protocol

`ExecutionLocations` is a mechanical metadata query selector: either all locations
or at most 257 distinct explicit locations (Local plus the product grant bound).
It is process-local bounded data, not a grant, provider lease or durable identity.
The consumer's product admission owns whether a selection may be used.

`ExecutionLocation` is either this Service's Local location or one stable SSH
`ExecutionTargetId`. A target identity is 32 lowercase hexadecimal characters;
only its owning target registry assigns it. It is neither a hostname nor an
OpenSSH alias, and it carries no authentication or permission.

`ExecutionCoordinates` pair that identity with a normalized absolute UTF-8 path
of at most 16 KiB. SSH paths use POSIX spelling. Local paths are lexical host
paths, including Windows spelling when decoded on another client. Deserialization
checks both members together and rejects unknown fields. It never probes the
reader's filesystem or establishes that the directory still exists. The target
filesystem owner must canonicalize a path before issuing these coordinates.

Connection epochs, target configuration revisions, provider generations and
live grants belong to execution leases, not durable location identity. Workspace
owns derivation of a workspace identity from the complete coordinates. Two target
identities with the same path remain different locations.

Execution bindings and plan identities are bounded non-authorizing correlation
data. Decoding validates Local versus SSH revision/epoch invariants and nonzero
process-local generations. Execution review combines one binding with an optional
positive plan sequence and an absolute bounded workspace in that location's
namespace. No deserialized value can construct a live lease or process plan.
