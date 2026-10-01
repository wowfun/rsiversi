---
name: Authorized SSH execution and helper lifecycle
---

## Problem

Authenticated workspace registration alone cannot establish permission to use
another machine. Reusing Service SSH configuration can run local commands,
and transferring the Service environment or arbitrary credential references
would export authority unintentionally.

## Decision

SSH builds on the [execution foundation](../architecture/2026-09-29-execution-location-foundation.md).
It extends the scoped grant machinery from
[Host Profile leaf management](../../implemented/architecture/2026-09-20-host-profile-leaf-management.md),
retaining typed operation scopes, Local issuance, revocation gates and draining.
Use, target management and target MCP stdio management are separate permissions.
Web submits bounded candidates; Local explicitly trusts the host key and identity
and grants access. Local Web launch does not implicitly add these grants.

The connector generates an allow-listed SSH configuration without reading user
or system config. Delegated target stdio configuration binds exact credential references. This
is a narrowly scoped extension of the Local-only decision in
[frozen MCP catalogs](../../implemented/feature/2026-09-15-frozen-mcp-catalogs.md);
local stdio and ordinary configuration grants retain their existing boundary.

Remote MCP processes outlive individual callers, as terminals do. A connection
therefore retains a duplex resource, while each bounded business exchange takes
current Use authority from the exact original provider. Checking only the creator's
lease would either lend its grant to later callers or incorrectly couple every
caller to its revocation. The reference MCP transport at
`.references/rsi/deepseek-harness/packages/mcp/mcp-client/src/transport.ts` delegates
stdio spawn directly to the SDK and scrubs a parent environment; it does not
establish this product's target-grant boundary. RSI uses the target account
environment and checks explicit credential references before resolving secrets.

Agent execution, history and model credentials remain on the Service. A thin app
starts reusable helper logic on Linux. Transport uses SSH exec stdio with bounded
credit-based multiplexing and reserved control capacity. Each lease pins its connection
epoch; uncertain effects are never replayed. A transient user systemd service owns
the helper cgroup, with a 30-second connection-fed watchdog and a two-second
termination grace. Admission requires cgroup v2 and the actual sandbox capabilities used.

The stdio transport fragments ordinary messages so a selected large message cannot
hold the writer ahead of reserved controls. Credits count frames rather than bytes:
tiny chunks must consume finite queue slots too. Pending request capacity survives
caller cancellation until a matching reply or connection retirement. Receive budgets
also survive delivery to a handler. Separate parity and monotonic identities avoid
an unbounded replay tombstone set while allowing controls to overtake ordinary calls.
Stream identities use bounded slots and generations for the same reason.

Cleanup must be admitted even when many views close together. Exhausting reserved
request capacity is an undispatched rejection, not evidence of a dead connection.
Unstarted plans retire unused local streams before releasing target reservations;
started processes do that only after verified native settlement. Otherwise a new
target stream generation can overtake local receipt of the previous cleanup reply.
Already in-flight historical Data/Credit cannot publish into a replacement slot.

Native process reaping and lossless output completion are distinct facts. A cancelled
duplex process can be fully reaped while its output consumer receives an interruption
error. Transport status carries both results, preserving the Process owner's separate
`wait` and `wait_settlement` contracts. Captured output retains whole-stream offsets;
target-local completed-output cache IDs cannot be exposed as Service cache IDs.

Reply publication has two independent boundaries. A client accepts a reply only
after dispatch of its complete request, so an early peer reply cannot certify an
unfinished operation. A helper releases its inbound slot before writing the final
reply fragment, because the peer may consume that reply before local flush returns.
The single selected frame remains separately bounded through the write.

The installer publishes immutable, digest-verified musl artifacts in a private
local-filesystem runtime cache, isolated by Service identity. Continuous artifact
leases coordinate publication and collection. Deploying an upgrade does not replace a live
helper; reconnect explicitly creates a new epoch.

The bootstrap verifies the uploaded image with the target's fixed SHA-256 tool
before chmod or execution. A self-check inside an unverified uploaded executable
cannot establish this boundary. The local connector retains immutable verified
bytes, its generated OpenSSH configuration and actual child reaping independently
of connect publication. Process clients and accepted work retain the connection;
heartbeat and reaper tasks hold no authority-owning reference back to that lifetime.
This prevents a background task cycle from keeping an abandoned connection alive.

The cache uses a persistent writer inode and independently opened artifact leases.
Lock guards explicitly unlock when their owner finishes; CLOEXEC alone does not
prevent an unrelated concurrent fork from temporarily retaining the open file
description. This follows the same ownership issue already handled by the product's
`writer_lock.rs`. Publication order uses bounded atomic metadata rather than mtime,
so wall-clock changes cannot choose the two retained versions. Unknown filesystem
types, including overlay whose backing store locality is not established, fail closed.

A candidate reserves a caller-selected random identity with revision CAS, so a lost
create response cannot allocate another target on retry. Candidate submission
uses ordinary configuration admission but grants no execution. The existing grant
owner persists exact target scopes and retains revocation drains; target management
holds those tokens through durable publication or bounded connection initialization.
Local remains the trust administrator. Identity file contents stay outside Storage;
Local confirmation records their digest, and connection preparation verifies and
copies the bounded private file into connection-owned storage before OpenSSH starts.

The installed helper loader verifies the adjacent distribution receipt against the
actual running executable and source-family digest. Helper bytes also require the
same CPU and a static ELF without PT_INTERP. This prevents a correctly hashed GNU
helper from accidentally depending on a target dynamic loader. Transient connection
CAS includes the observed epoch as well as the durable target revision: reconnect
does not change target configuration, so revision alone cannot fence a stale close.

Heartbeats prove peer liveness, not progress of an accepted RPC. Each request
therefore has an independent absolute deadline even after its waiter disappears.
Expiry retires the connection epoch: removing only the pending entry would lose
the reply's effect classification and leave remote resource ownership uncertain.
Queued work remains undispatched failure; dispatched work remains outcome unknown,
and cleanup proceeds through the existing connection and helper lifetime owners.

## Alternatives considered

An installed full RSI Host on the target moves history and provider credentials.
OpenSSH aliases permit local configuration commands. Separate forwarded sockets
would require additional peer authentication. PID groups alone cannot establish
the required descendant-cleanup guarantee.

## Consequences

A device without Use cannot register, execute or browse a target through any
Session, picker, file or terminal path. Credential and SSH configuration probes
show no unintended Service authority export. Native Linux tests cover lost
responses, helper death, missed heartbeats, setsid descendants, target sandbox
failure, epoch replacement, concurrent Services and no replay. The initial Web
flow includes trust status, deployment, connection and a target directory picker.

This does not isolate a malicious target administrator or another process with
equivalent account administration authority. Cleanup covers the owned cgroup,
not external service effects. Hosts without the required runtime directory,
user manager, watchdog or sandbox fail closed. SSH ACP remains unsupported.
