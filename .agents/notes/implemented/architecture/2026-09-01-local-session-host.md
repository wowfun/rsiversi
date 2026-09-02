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

Callers allocate `TurnId`. The Kernel fingerprints the canonical accepted body,
persists the Header and acceptance boundary atomically, returns the durable
sequence as a receipt, returns the same receipt for an identical retry across
reconnect or restart, and rejects a changed body as a conflict. A fresh handle
retains its exact composition pin until durable acceptance so a failed
pre-durability attempt does not resolve another generation.

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
frame. History, recent sessions, and approvals are sequences with one typed
item per frame. A process-wide 64 MiB decoder ledger is acquired from the
declared frame length before allocating raw bytes, and the handshake has its own
16 KiB ceiling. Clients reject a sequence beyond both the requested operation's
count/variant contract and its aggregate semantic byte bound. Returned Session,
Turn, history-Fact, approval, and subscription-event identities are carried on
the wire and compared with the initiating operation. Subscriptions carry at
most one Fact per event frame. An established subscription may wait indefinitely
between frames, but after receiving a frame length the client bounds decoder-ledger
admission and the remaining body read to 30 seconds.
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

Approvals are live Host-generation state. Every capable client attached to the
Session sees the same bounded pending set, the first valid answer wins, and
cancellation or Host shutdown removes the request. Facts remain the only
reconnectable history and unfinished external effects are interrupted rather
than replayed after recovery.

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

Session format 4 and Store schema 7 are exact pre-release formats without a
migration path. This keeps one current contract rather than preserving an
unshipped schema. Hard process death cannot guarantee cleanup for an
unconfined descendant that escapes its process group with `setsid(2)`; the
restricted Linux sandbox still binds its supervisor to parent death.
