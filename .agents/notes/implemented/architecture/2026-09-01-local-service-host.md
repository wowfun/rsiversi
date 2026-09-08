---
name: Product-wide local Service Host
comment: One standard-path owner with independent domain APIs and native process control
---

## Problem

The Agent Kernel owns durable session truth and turn control. Product clients
also need to reconnect and use Workspace, Models, Settings, Media and completed
Output independently of a Session. Starting a Host per working directory would
duplicate global Store and composition ownership even though Workspace already
partitions canonical working directories. A Session-owned client or wire adapter
would give one domain authority over the whole product.

## Decision

The former Session-specific wire design is superseded by the
[application/client foundation](2026-09-06-application-client-foundation.md).
The [API contracts](../../../../crates/rsi-api/README.md) now own operation
admission, mutation retention, framing and delivery budgets; this decision
retains the native owner, discovery and lifecycle rationale.

The standard product owns one Service Host per canonical HostPaths identity.
Session is an ordinary domain plugin; its contracts remain at the
[Session boundary](../../../../crates/rsi/session-protocol/README.md). Applications
consume that capability alongside other independent domains. Native process
ownership lives in `rsi-service-host`; generic composition remains in `rsi-host`.
The shared [API foundation](../../../../crates/rsi-api/README.md) owns dispatch,
connection negotiation and transport resource accounting.

Embedded and daemon owners acquire the same persistent lease. The owner plugin
activates before durable providers. The identity plugin then uses independent
Storage to publish EndpointId durably and to reject corrupt identity data without
replacement. EndpointId identifies a deployment across restarts; a fresh HostEpoch
identifies one running generation. Separating them preserves device credentials
without making a previous process generation current again.

A daemon publishes recoverable metadata after its complete Profile is active.
Its ordinary Local API listener owns private socket publication and connection
tasks. Independent UDS and domain client plugins share the API codec and operation
registrations. A compatibility key combines the exact executable build with the
product launch key; same-UID peer credentials remain the local authentication
boundary. Remote wire negotiation does not weaken this automatic local reuse gate.
Readiness requires completed description and catalog operations, without touching
Session creation or attachment.

The launch key identifies the desired shared service composition: protocol epoch,
product version, composition epoch, frozen catalog, Host Profile authority,
Agent preset selection and roots, and Linux coding executable identity. It excludes
current working directory, application arguments, Session/Turn identities, current
Host Profile contents, secrets, child environment and native publication role.
Embedded and daemon owners therefore select the same service deployment. The
listener's runtime launch-key configuration is added after pure service preview;
its factory already belongs to the frozen native catalog. Reload can change the
active Profile contents while reporting restart-required for frozen owner changes.
A forged user Profile authority shape is rejected before deriving the key.

Listener generations are shorter-lived than the process owner. Replayable
Profile changes retire an ordered suffix that can include the registry, device
verifier and local listener. The daemon observes Profile convergence and resolves
the replacement listener; normal retirement cannot decide the owner's lifetime.
Parent teardown may fence the owner before Profile cleanup publishes Stopped.
The Serve composition resolves current capabilities for each new admission,
while admitted operations retain their original registration and revocation
authority. Weak composition references avoid retaining an obsolete owner through
escaped application handles. Restarting the process on every listener retirement
would defeat supported reload; merely ignoring retirement would leave HTTP and
diagnostics bound to stopped services.

Client selection connects to a compatible daemon, embeds only after acquiring an
unowned lease, and waits within one bounded readiness interval for startup or a
temporarily unresponsive owner. It never bypasses a living incompatible owner or
autostarts a daemon. The daemon's recorded endpoint is authoritative even when a
client has a different or unbindable runtime-directory preference.

Metadata validation is independent of executable compatibility. Schema 2 records
the API deployment identity; schema 1 remains readable for lifecycle control of
an older exact process. Linux lifecycle signals open a pidfd and recheck the
recorded process-start token before signaling that descriptor. Retaining the
persistent `session-host` lock-directory name prevents a renamed binary from
bypassing an older process that still holds the original lock.

Detached startup acquires the owner lease before opening its log or spawning.
The unpublished lease moves through the child's stdin descriptor without an
unlock/relock interval. The child validates the inherited file against its exact
owner path and makes stdin close-on-exec before boot. Failed spawn or child exit
releases ownership without stale starting markers. Socket publication uses a
private staging endpoint and hard-links the verified inode into place. Stale
removal requires a failed liveness probe; cleanup checks device/inode so it cannot
silently remove a replacement socket.

Domain semantics remain independent of transport. The Kernel owns MessageId
acceptance, exact-body conflict detection and durable status reconciliation. The
Session plugin owns shared draft admission and composition pins. Brokers own
live approvals and questions, including bounded settled-answer receipts. Reopening
a connection does not recreate a waiter or replay an unfinished external effect.
The [Agent Kernel decision](2026-08-26-durable-agent-kernel.md) and owning contracts
retain those rules without duplicating them in a native wire implementation.

## Alternatives considered

Working-directory ownership duplicates a partition already owned by Workspace.
Daemon autostart hides a background ownership mutation inside ordinary application
launch. A wire-shaped application API spreads framing and reconnect policy into
each UI. The original local-only scope did not need HTTP or WebSocket; the accepted
[application/client foundation](../../implemented/architecture/2026-09-06-application-client-foundation.md)
adds explicit multi-device access with separate authentication and TLS contracts.
Session identity remains unsuitable as authentication in either scope.

Persisting live approval waiters cannot restore the exact external effect that
requested their decision. A generic durable outcome ledger would duplicate domain
truth: MessageId/status, draft coalescing, Settings revisions and bounded interaction
receipts already identify the applicable reconciliation authority. Disconnecting a
waiter cannot retract a mutation that the API has admitted and started.

## Consequences

The service process remains independent of remote client lifetime. Local API
plugin cleanup cancels and drains transport tasks; operation retirement separately
drains admitted mutations. Control, data and subscription budgets are independent,
so idle observers or slow history readers cannot consume cancellation capacity.
Response retention and encoding scratch also have separate owners. These bounds
cover retained application buffers, not total process RSS or delivery after a
connection disappears.

SIGHUP reload work remains separate from signal handling so TERM/INT can initiate
shutdown during reload. Native socket drain is bounded to one minute; lifecycle
control allows a further Host-shutdown margin. The daemon remains Linux-only until
other platforms have exact process-generation control and native verification.
Linux checks do not establish native Windows/macOS behavior. An unconfined
subprocess that escapes its process group with setsid can outlive hard process
death; the restricted Linux sandbox binds its supervisor to parent death.

Media import independently publishes a canonical object. A later failed or uncertain
Message may leave it unreferenced; rollback deletion could break another Session.
Cross-service reference tracking and collection remain a separate milestone.
Session and Store formats are unchanged by the client refactor; their current
versions and validation belong to their protocol and Store owners.

Native and browser transport fixtures
establish their stated protocol and lifecycle behavior; they do not substitute
for product visuals or live model verification.
