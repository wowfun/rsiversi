---
name: Product-wide local Session Host
comment: One standard-path owner with local and same-user UDS Session adapters
---

## Problem

The durable Agent Kernel owns append-only historical truth and process-local
turn control, but a product client also needs to reconnect, page history, list
sessions, and answer live approvals without learning Kernel preparation tokens,
Store cursors, composition generations, or transport framing. Starting one
Host per working directory would duplicate the global Store and composition
owners even though Workspace already partitions canonical working directories.

## Decision

The `rsi` product owns a transport-independent `SessionApplication` and
`SessionHandle`. The local adapter composes Kernel, Store, Workspace, frozen
settings, AI routes, and approval control; the UDS adapter carries the same
operations over strict length-prefixed JSON. Both adapters run against the same
real Kernel and SQLite behavior contract.

One canonical standard `HostPaths` authority has exactly one owner. Embedded
applications and explicit daemons acquire the same persistent lease. A daemon
publishes a private same-user socket only after its Host has booted, and clients
must match protocol epoch, product build, `HostLaunchKey`, and random
`HostEpoch`. Durable owner metadata validates its bounded document shape without
requiring compatibility with the reader's executable; that separation lets a
rebuilt lifecycle client identify, inspect, and signal the exact older process.
The daemon's recorded endpoint is authoritative for that generation, so reading
metadata never depends on whether the client's preferred `XDG_RUNTIME_DIR` could
host a socket. Client selection connects to a compatible daemon, embeds only
after acquiring an unowned lease, waits up to the same 15-second readiness bound
for a starting or temporarily unresponsive owner, and never autostarts or
bypasses a living incompatible owner.

Callers allocate `MessageId` for Language and multimodal input. The Kernel
fingerprints the canonical message, atomically persists its control acceptance
and the Header when needed, returns the indexed durable state for an identical
retry across reconnect or restart, and reports a typed conflict for a changed
body. Acceptance returns no speculative Turn identity. A later claim atomically
creates the activation, Turn, Step, and model-visible input; message status and
dual-stream observation expose that binding. Direct Image generation remains a
separate caller-allocated `TurnId` operation. A fresh handle retains its exact
composition pin until durable acceptance so a failed pre-durability attempt does
not resolve another generation. If another publisher wins the fresh-Session
race with the exact same immutable Header, the losing handle reconciles that
durable Header and becomes attached; a different Header remains an error.
Only the first durable publication is serialized by the handle state lock.
Once attached, the state carries no mutable generation pin, so independent
Language or Image submissions release that lock before resume preparation and
backend I/O; Kernel admission remains the authority for same-Session ordering.

Application and Host Profiles are closed, bounded, non-shadowable catalogs.
The launch key includes the Session Host protocol epoch, declared product
version, composition epoch, Host composition, Host Profile identity and
authority root, Agent preset default and roots, and Linux coding executable
identity. The handshake's product build separately includes the exact running
executable digest. The launch key excludes current working directory,
application arguments, Session/Turn identities, current Host Profile contents,
credential secret bytes, and the process-local child environment frozen by the
owner. A caller-supplied user Profile document without the catalog's
`host-profiles/<id>/host.profile.toml` authority shape is rejected instead of
panicking while deriving that root. Reload recompiles the active Host Profile
but reports restart-required when the frozen owner generation changes.
Profile preview derives the launch key from the same Settings-backed
Agent-preset identity as daemon launch while avoiding built-in asset
materialization and Host activation.

The wire uses four-byte big-endian lengths and one strict JSON value per bounded
frame. A bounded shape pass recursively rejects unknown fields in every locally
defined tagged frame, operation, response, item, update, and error before typed
decode; the temporary shape is released before constructing the typed value.
History, recent sessions, approvals, and observations are sequences with
one typed item per frame. Ordinary frames share a process-wide 64 MiB decoder
ledger acquired from the declared length before allocating raw bytes, and the
handshake has its own 16 KiB ceiling. This bounds retained wire bytes, not total
heap use: the temporary JSON shape and typed values have additional allocation
overhead within each frame's bounds. Multimodal input declares each upload's
exact byte length and SHA-256 digest, then sends canonical-base64 chunks in
contiguous indexed JSON frames below the small upload-frame ceiling. Every
nonfinal chunk contains exactly 48 KiB of decoded input; the final chunk is the
exact remainder, bounding frame count by declared bytes and content-block count. A separate
64 MiB upload ledger is acquired from the aggregate declaration before any body
is retained. Empty or oversized content-block lists are rejected before any
upload body is read; malformed lengths, digests, indexes, base64, or
reconstruction produce a typed error response before Session admission. One absolute one-minute deadline
covers the complete upload rather than restarting per frame, and the decoded
request retains its frame-memory permit through dispatch. A response timeout
after the complete message request can be applied, or a matching response with
neither or both result arms, returns an explicit unknown-outcome error carrying
the exact caller-owned Message identity for later reconciliation. The CLI queries
that identity first, resubmits the exact input once only if absent, and queries
again if the retry outcome is unknown. Failed queries retain the unknown-outcome
identity in the error. Connect and
handshake failures before transmission remain
ordinary backend failures and do not trigger outcome reconciliation. Clients reject a sequence beyond
both the requested operation's count/variant contract and its aggregate semantic
byte bound. Returned Session, Message, Turn, history-Fact, approval, and
observation identities are carried on the wire and compared with the initiating
operation. For tree-wide approvals the client reads each distinct subject
Session's immutable Header and checks its root against the initiating handle;
the server still owns live approval routing. An established observation may wait indefinitely between frames,
but after receiving a frame length the client bounds decoder-ledger admission
and the remaining body read to 30 seconds.
Request IDs, connections, unpublished drafts, pending approvals, page sizes,
and writes are bounded. The server reserves draft admission before invoking
application creation, and the map entry itself retains that permit. Successful
submission and a conflict both establish durable Session identity and therefore
remove the unpublished entry. An unpublished draft is otherwise an idle lease: creation and
every operation through that draft renew it for one hour, while expiration
makes it eligible for the server's one-minute reclamation sweep. Reclamation
drops the retained composition pin, and a later operation resolves through the
ordinary durable attach path. Socket publication uses a private staging endpoint
and hard-links the verified inode into place; stale removal follows a failed
liveness probe, same-user peer credentials are mandatory, and cleanup rechecks
device/inode. Completed connection tasks are joined during ordinary serving so
retained task bookkeeping is bounded by the live connection limit rather than
historical connection count.

Every connection still carries exactly one request after its handshake. A
readiness client sends a side-effect-free `Probe` on the already-handshaken
stream and requires the matching `Ready` response within the short control-plane
deadline; this proves request handling is live without creating or attaching a
Session. Message receipts include the durable Fact tail observed beside their
control state, so the CLI can wait for `MessageClaimed` on one reconnectable
subscription rather than opening a new connection every 200 ms. Host protocol
epoch 4 fences that receipt addition.

The server exposes a cloneable diagnostics handle with monotonic, saturating
counters for transport failures and admission rejections. Connection tasks
return typed failure stages, and both live reaping and bounded drain inspect
their completion so panics and forced aborts are not silent. The library does
not print or retain payloads. The standard daemon emits periodic deltas only
when an anomaly counter changed, plus one final delta to the owner log;
successful traffic alone is silent. This is Session Host observability, not a
claim that every Agent or Store background failure is covered.

Approvals are live Host-generation state. A capable client attached to an Agent
tree root lists pending approvals across that exact durable tree and routes an
answer to the approval's exact Session subject; an ambiguous identity is
rejected. Unknown or already-removed approval identities return `false`; the live broker
does not retain a history to distinguish them. The first valid answer wins, and cancellation or Host shutdown removes
the request. Facts and Agent-control records are reconnectable history, while
unfinished external effects are interrupted rather than replayed after
recovery.

Relative workspace paths belong to the calling process. The UDS adapter
canonicalizes a draft workspace before encoding it, and the server accepts only
an absolute wire path before the local Session adapter revalidates the canonical
directory. A daemon therefore never interprets a client's relative path against
the daemon's own launch directory.

## Alternatives considered

Working-directory ownership was rejected because Workspace already owns that
partition inside one Host. Daemon autostart was rejected because ordinary
application launch must not hide a background ownership mutation. A wire-shaped
public API was rejected because it would leak framing and reconnect policy into
every application. HTTP, WebSocket, TCP, and Session identity as authentication
were rejected because this is a local same-user control plane. Persisting live
approval waiters was rejected because restoring a waiter cannot safely recreate
the exact external effect whose one-shot decision it controlled.

Detached startup acquires the owner lease in the launcher before opening its
log or spawning. The unpublished lease moves through the child's stdin file
descriptor with no unlock/relock interval. The child validates that inherited
file against the exact owner path and marks stdin close-on-exec before any
bootstrap work. Daemon construction consumes that lease explicitly, so embedded
selection sees startup as occupied before metadata is ready. A failed spawn or
child exit releases the inherited file without stale starting markers.

## Consequences

The named `session` and `headless` Application Profiles replace the old direct
run surface. Explicit profile-management and Host lifecycle commands own their
mutations. A graceful daemon stop halts admission, interrupts approval waits,
drains clients for at most 60 seconds, and then shuts down the Host. SIGHUP
reload work runs independently of the signal loop, so TERM/INT begins that drain
even while a reload is in flight. The lifecycle client polls graceful teardown
for the 60-second drain plus a 15-second Host-shutdown margin. The standard
product daemon is Linux-only because lifecycle signals open a Linux pidfd and
then recheck process start identity before signaling that exact descriptor;
other platforms remain embedded-only. Native
Windows/macOS behavior is outside the Linux-local verification claim.

Multimodal submission imports complete bounded images into content-addressed
Media before atomically committing the Agent Message. If a later import or the
Store commit fails, an earlier object may remain unreachable from Agent history;
it cannot be deleted as rollback because another Session may already reference
the same digest. No partial Message becomes visible. Removing this consequence
requires a cross-service staging, commit, and abort interface or durable
provisional Media handles rather than a local transport change.

Session format 6 and Store schema 11 are exact pre-release formats without a
migration path. This keeps one current contract rather than preserving an
unshipped schema. Hard process death cannot guarantee cleanup for an
unconfined descendant that escapes its process group with `setsid(2)`; the
restricted Linux sandbox still binds its supervisor to parent death.
