---
name: Explicit homes and shared product launch
---

## Problem

Application-specific publications and embedded owners make adding an interface
change the executable and its Service identity. The first launch also hides
configuration, credentials and execution-environment ownership from later clients.

## Decision

The product launch contract uses explicit config, state and cache homes, with
ordinary platform defaults. Debug and release use the same resolver. The launch
document references Profiles; Settings remains machine-written JSON. One owner
leases both config and state before opening providers. Different pairs cannot
share a writable home.

Deployment identity, process epoch, core artifact identity, effective Service
revision and interface compatibility are separate. Lifecycle status and stop
remain available without loading configuration or matching a core artifact.
Applications connect to an automatically started persistent Host. ACP carries
its existing bounded protocol to a connection-owned backend inside that Host.

Host execution uses its launch environment and explicit configuration. Credentials
are independently editable; later terminals do not silently replace either.
Windows daemon admission requires native peer/process fencing and a detached
runtime copy that does not lock the developer's output executable.

Both reset options operate under exclusive maintenance ownership, preserve
credentials and installed packages, and archive only explicitly owned data.
Recoverable journals and ownership locks never move with reset targets.

This decision partially supersedes
[explicit Host launch](../../implemented/architecture/2026-09-01-local-service-host.md),
[application ownership](../../implemented/architecture/2026-09-26-application-ownership-and-paired-web.md)
and [Windows onboarding](../../implemented/architecture/2026-10-16-source-onboarding-and-windows-applications.md).
The superseded clauses no longer describe ordinary product launch; the earlier notes retain their independent domain and platform rationale.

## Alternatives considered

Deriving the configuration root from a selected file moves credentials and user
instructions unexpectedly. Writing Settings into TOML loses JSON values and
creates competing editor/provider writers. Automatically forwarding every client
environment requires a separate Session execution and recovery contract.

## Consequences

Closing a UI leaves the shared Host and admitted work alive. Explicit Host stop
is the authority for process retirement. Auto-start therefore makes background
ownership visible through status, native process identity, logs and bounded
startup failure, instead of retaining an embedded fallback that changes lifetime.
The original concern that autostart hides a mutation is addressed by these
observable controls; it is not a reason to maintain two ordinary runtime models.

The [launch contract](../../../../crates/rsi/core/docs/launch.md) owns paths,
reset and explicit import. Import preflights selected inputs, copies into private
staging, verifies the Agent database through its owner, archives previous owned
data and publishes a recoverable journal. External Profile paths and packages
are not silently copied or rewritten. Reset and import can cross volumes, so
publication is recoverable rather than one atomic rename. Credentials have their
own owner; reset preserves them and explicit import can copy their selected tree.

Large temporary copies need durable ownership before copying: an in-memory
cleanup guard cannot recover a crashed preparation. Import therefore separates
discardable preparation from the final recoverable publication intent. Reset
archives are user recovery data; exact participant lookups must not be limited
by the number of unrelated archived siblings.

Windows owner control uses private named pipes, native PID/SID/start-time checks
and an immutable executable copy. A Job that forbids detachment fails explicitly.
Same-user transport is not isolation from hostile unconfined processes. SDK
embedding remains explicit. Native macOS ownership is not inferred from Windows
or Linux verification.

Only the ready Host generation needs an immutable runtime copy. Startup can
reclaim inactive generations while holding its coordinator. Cached executable
bytes are still hashed: equal length and modification time do not establish
integrity, and are insufficient grounds for admitting an executable.

Focused tests cover home contention, damaged-document recovery, journal steps,
source unavailability after import staging, ACP attachment retirement and owner
persistence. Product visuals and live provider calls remain separate acceptance
from these deterministic tests.

The launch deadline is three minutes, including immutable debug executable
publication. Integration waits allow an additional thirty seconds to collect
cleanup and diagnostics; they do not define a separate production deadline.
Embedded application launch resolves the same selected Service reference when
the platform has no resident launcher.

Product resets have one exclusive maintenance entry. The obsolete composition
reset injection is removed; standalone SQLite reset remains owned and tested by
the Store adapter. This includes the public factory reset option and one-shot
request wrapper: retaining them would keep an unused second startup authority
and receipt lifecycle. The standalone receipt, writer exclusion and archival
preservation contract remain independent of product startup.
Whole-topology reset preflight prevents discovering a known
ambiguous later participant after moving an earlier one. Import likewise carries
validated classifications through a single rename-only settlement under leases;
a new recovery invocation re-admits all durable bytes. This reduces redundant
I/O without substituting timestamps for integrity checks.

The native close protocol applies to Host loss as well as explicit window close.
The Worker and native lease can fail in either order, so local drafts must flush
even when the transport already failed. A failed flush cancels native exit.
