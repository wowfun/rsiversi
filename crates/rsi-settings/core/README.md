# rsi-settings

This ordinary plugin owns the active Settings namespace registry. It loads one
complete raw provider document before publication, validates every namespace
at registration, and resolves objects recursively in `defaults -> base -> user`
order while arrays and scalar values replace as complete values.
Registration runs the namespace validator outside the registry mutex, then
rechecks namespace availability and its raw snapshot before publication. A raw
value changed by a concurrent namespace generation returns `StaleRegistration`.
Revision exhaustion is rejected before durable write admission.

Validation and definite prepublication failures leave the value and revision
unchanged. An uncertain publication returns `OutcomeUnknown` and marks only its
namespace for reconciliation. While holding write ownership, the service makes
one provider read, bounded to five seconds, and validates the observed section
against the namespace spec. Success replaces that section and advances its
reserved revision even when the value is unchanged. This observation does not
turn the original failure into a durability receipt. Failed reconciliation
leaves synchronous reads, descriptions and writes returning `RecoveryRequired`;
an explicit asynchronous read makes one further attempt. Other namespaces remain
usable. Recovery markers survive registration retirement; an unregistered read
may repair the raw cache but still returns `UnknownNamespace`. Registration must
wait for recovery; asynchronous registration may perform that recovery first.
Once a durable write begins, a service-owned operation completes its
live-state publication even if the requesting future is dropped. The complete
raw document is the sole retained raw-section authority and is updated with the
same commit. CAS captures it after checking registration identity; later registration cannot
reload stale activation-time state. Dropping a registration lease makes all
escaped scopes stale and defers namespace handoff until any in-flight commit
has converged. A publication guard fences uncertain state on provider panic or
task cancellation while releasing in-flight ownership. Migration uses the same
reconciliation rule and never converts an uncertain write into a successful
registration or automatically replays it.
Final namespace destruction runs outside the registry lock, both on immediate
withdrawal and after a retained commit converges. Validators may own dependent leases.
Meta withdrawal removes service discovery and does not revoke an escaped typed
registry handle. Namespace leases still own scope validity and handoff as above;
this follows Meta's [local-service lifetime](../../rsi-meta/core/README.md).

The same plugin publishes asynchronous `SettingsAccess` for explicit client
namespace projections. Registration allocates a fresh opaque scope identity;
snapshots carry it alongside their revision. Versioned writes compare identity
within the namespace lookup without copying a resolved value, then retain that scope's generation fence
through validation and durable commit. Re-registration and service restart
cannot reuse an earlier client's revision-zero authority.

The namespace owner supplies validated descriptive metadata at registration.
Discovery omits retiring entries and unregistered provider sections. Listing
retains only the requested page plus one lookahead name; describing one namespace
captures its version, defaults, metadata and provider writability under the same
registry lock. It neither acquires a registration lease nor changes a revision.

An owner may register asynchronously with a pure raw-section migration. The
registry serializes migration with writes, reserves the namespace against other
registrations, validates the transformed merged value before provider CAS, and
publishes only after persistence. Failed migration publishes no namespace. A
transient CAS conflict follows the protocol's [bounded migration retry](../protocol/README.md)
and refreshes only the migrating namespace; unrelated published scopes are unchanged. A
read-only provider rejects a migration requiring a write. Once persistence starts,
the registry finishes updating its raw cache even if the caller disappears;
no registration lease survives a dropped result. Namespace owners cannot bypass
the registry to modify its provider.
