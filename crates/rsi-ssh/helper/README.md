# rsi-ssh-helper

Configured executable resolution performs native lookup outside the catalog
mutex and rechecks catalog deduplication and budgets before publication. The
validated target account is shared for the helper process lifetime; distinct
policies do not repeatedly launch account lookup.

Captured output drains each bounded native tail snapshot once into transport
fragments, preserving whole-stream offsets and reported gaps. It never rereads
the unsent remainder for each fragment or substitutes a newest-only tail read.

Reusable Linux helper mechanics belong here; executable argument handling and
distribution belong to the application layer. `TransientUnit` constructs only the
fixed user-systemd invocation. Its name binds a 32-character lowercase hexadecimal
Service namespace and a nonzero connection epoch. It cannot name or take over an
existing helper. The caller retains its artifact lease and actual launcher process.

An explicit source-reader preparation requests Sandbox's read-only host view. It
requires ReadOnly pipes, Host scratch and isolated networking; the helper checks
these constraints before planning. Ordinary preparation never gains this view
from an executable name or argument. The enabled immutable project collector and
read-only Git inventory use it so ancestor files in host scratch remain visible.
Programs still must match this connection's finite resolved program/environment
catalog. Project source parsing remains in the application-linked library.

The launch contract is `systemd-run --user --pipe --wait --collect
--expand-environment=no`, with Type=notify, NotifyAccess=main, Restart=no,
ExitType=main, RemainAfterExit=no, WatchdogSec=30s, WatchdogSignal=SIGTERM,
TimeoutStartSec=10s, TimeoutStopSec=2s, TimeoutAbortSec=2s,
KillMode=control-group and SendSIGKILL=yes. Runtime bus discovery uses the target's
`/run/user/<uid>`; it does not copy a Service-side bus or runtime path.

Before READY, `SystemdWatchdog` checks the live user manager's watchdog setting,
the exact unit properties, MainPID, unified `/proc/self/cgroup` membership and
the cgroup v2 filesystem. It verifies the current process's systemd watchdog
environment and notification socket against the target runtime directory. Bounded
systemctl reads use an empty environment except the derived target runtime path;
timeouts kill and reap the accepted inspection child independently of waiter loss.
Missing or mismatched prerequisites fail closed.

READY does not feed the watchdog. Only newly observed monotonic client heartbeat
serials from the bound helper-role transport send WATCHDOG=1. A local timer never
feeds it. Stopping or stalling that loop therefore leaves systemd responsible for
terminating the whole owned cgroup, including descendants that call setsid.
Connection closure stops feeding. Callers must settle their process registry and
exit; lifecycle verification itself grants no workspace authority.
It does not replace the independent sandbox/openat2 probes or artifact cache lease.

Default tests exercise bounded parsing and fixed command construction. Ignored
Linux lifecycle tests create private transient units with the actual user manager;
they deliberately exercise watchdog expiry and descendant cleanup. They require
no model credentials, SSH server, native Windows or macOS environment.

## Artifact cache

`ArtifactCache` opens an existing private 0700 runtime directory through no-follow
directory handles, and requires the caller UID, an executable writable mount and
a recognized local filesystem (tmpfs, ext, XFS or Btrfs). Unknown filesystems,
network mounts and overlay mounts fail closed; an overlay type alone does not prove
its backing store is local. Cache directories are private and separated by Service
namespace. Each artifact is at most 128 MiB, named by its SHA-256, published by an
initial atomic no-replace rename, and stored as a single-link 0500 regular file.
Mount policy is checked on the runtime root, family directory and actual Service
cache directory; a nested mount cannot inherit approval from its parent filesystem.

One persistent writer-lock inode serializes cache mutation, lease acquisition and GC.
Callers select immediate admission or an absolute writer-lock deadline. Only writer
flock contention waits; the deadline is never refreshed. Native install and serve
each allow two seconds, backing off from one to at most fifty milliseconds.
Artifact-lease contention can occur after publication and is not replayable.
Writer timeout is reported as CacheContentionTimeout, including helper exit code
75 through the launcher. That diagnostic does not establish absence of remote effects.
Each acquisition opens its
own lock description; cloned descriptors do not accidentally share lock ownership.
Owned lock guards explicitly unlock at completion, so unrelated concurrent forks
cannot extend an operation merely by briefly inheriting its CLOEXEC descriptor.
Artifacts themselves carry shared advisory leases. Acquisition and publication take
the shared inode lease under the writer and release the writer before hashing;
no unverified lease or pathname is returned. Staging checks the complete incoming
digest before rename. Publication records its order and opens the shared lease in
that same writer critical section, then hashes the leased inode after releasing
the writer. Other publishers and GC can progress during this final verification
without treating an in-flight publication as an orphan or deleting its inode.
A final verification failure can follow committed cache mutation; it does not
establish absence of publication effects. Publishing again with correct bytes
repairs a digest-mismatched regular artifact: after verification fails, one new
writer admission and an exclusive artifact lease permit a verified staged image
to atomically replace the inactive inode. A live shared lease refuses replacement;
the writer deadline is not refreshed and the incoming image is not replayed.
Unsafe file shapes or unknown entries fail closed rather than being automatically
deleted. An operator must stop the Service's helper units and release its leases
before removing that private cache namespace and reconnecting.
GC can unlink one only while
holding both the writer lock and an exclusive artifact lock. The writer lock is
never unlinked, and live artifact inodes are never replaced or unlinked.
Every operation also verifies the current cache pathname still names its pinned
directory before acquiring a writer or issuing a lease. The cache pathname is diagnostic data;
execution uses the live launcher's `/proc/<pid>/fd/<fd>` path to the verified inode,
retained until the systemd-run child settles. Directory replacement cannot redirect
that execution. The local kernel, procfs and target account remain trusted.

A healthy cache hit verifies the cached inode without consuming incoming bytes;
input is staged and digest-verified only for installation or inactive repair.
Staging failure precedes artifact collection. Publication is not a transaction:
later native failures can leave synchronized cache or metadata changes.
A bounded atomic state file records the latest publication order, including reuse
of an existing version, independently of wall-clock
changes. GC retains active artifacts and the newest two known versions. Interrupted
publication can leave an immutable unindexed artifact; it is treated as older than
recorded publications, while its live lease still prevents deletion. Interrupted GC
may leave missing names in the state file; the next writer reconciles them. Malformed
metadata is rejected. Unchanged metadata is not rewritten; artifact publication
and collection retain their file and directory synchronization barriers.
There are at most 32 artifacts per Service; a full set of live
leases prevents new publication. Staging files have fixed bounded names and are
cleaned only under the writer lock.

The launcher keeps its returned lease until the helper acquires its own lease and
acknowledges that acquisition. Cache operations support that overlap but do not
perform the transport handshake. A path or digest alone is not a lease. These are
blocking, byte-bounded native operations; the caller retains their worker through
completion and supplies any operation deadline. They never claim arbitrary disk or
input-reader work has a hard wall-clock deadline.

## Execution server

The connection server receives an explicit native capability tuple and finite
program catalog. It owns at most 20 prepared or running processes, using three
of the transport's stream slots each. Plans are one-use and bound to the issuing
connection. Preparation registers output senders before returning their identities;
start registers batch stdin only after the client has registered its sender.
Program selection and environment come from target policy, never ambient Service
state. The server checks preparation against a catalog program/environment pair.

Interactive input uses bounded requests that acknowledge the native accepted
prefix. Stream credit is not a write acknowledgement. Batch input uses the reserved
stream and must match its declared bounded length before native spawn. Status
queries wait for outcome and settlement for at most five seconds. One monitor per
running process uses at most 20 of the 32 ordinary slots; cleanup shares that
monitor once Start has returned. Terminate uses the separate control pool, starts
native escalation and acknowledges promptly; settlement remains a separate fact.
Captured output streams include absolute byte offsets, preserving loss when the
native tail advances. Lossless duplex and PTY output use their native byte ports.
Remote capture initially has no completed-output cache identity; a target-local
cache ID must never be published as a Service-local cache ID.

Connection closure cancels uploads and output pumps, terminates native processes,
joins admitted handlers and reaps accepted children. Native duplex backpressure
settles within 500 ms with an exact prefix or a
pre-effect Capacity rejection; it cannot consume the connection RPC deadline.
Request waiter loss does not cancel an admitted native effect. Process release is allowed only after native
settlement; prepared-plan cancellation releases its reserved streams. These mechanics
do not grant target access or replace the systemd cgroup owner's final cleanup.

Files requests use the same connection and native directory-handle provider.
The server binds each handle to an opaque connection caller and exact workspace,
retains at most the Files owner's token bound, and releases that caller when all
connection handlers finish. Reads and directory pages retain the native version
checks, including executable bits. Opening sweeps expired registry entries;
read/list lookup checks only the selected native handle, without restatting all
other open files. No request opens a Service-side path.

## Native entry and initialization

The application accepts only install/launch and serve modes with a fixed Service
namespace, epoch and artifact digest. Install/launch hashes its own image into the
private cache and retains its artifact lease through the systemd-run child. The unit
executes through that lease's pinned descriptor. Serve
acquires an independent cache lease before accepting execution, verifies systemd,
and probes Bubblewrap plus the actual restricted PTY/openat2 path before READY.
The helper owns binary stdin/stdout exclusively. Native nonblocking descriptor
adapters avoid a blocking stdin task keeping Tokio shutdown alive after retirement.
The launcher never reads or writes these byte ports while the unit owns them.

Initialization carries a finite program policy over the same bounded mux. Program
names resolve against a fixed target PATH; explicit absolute target executables are
also accepted. HOME comes from the target account database, and ordinary child
PATH/HOME cannot be replaced by policy extras. Missing selected dependencies are
reported unavailable; they do not trigger Service-side execution or ambient PATH
search. Extra child variables are explicit policy inputs, still excluding lifecycle,
DBus and agent keys. Grant and credential-reference narrowing precedes this library.

The optional built-in `apply_patch` selector uses the currently leased helper
image and an empty child environment. Its application dispatches the exact patch
marker to the Apply-Patch family's engine before ordinary SSH lifecycle argument
parsing; it does not run a shell or reinterpret the patch in this family.

An explicit target-program resolution may extend the connection's finite resolved
catalog without replacing existing selectors. Each selection is a validated
absolute target command or basename and explicit extra environment. HOME, PATH,
USER and LOGNAME come from the target account; SSH agent, systemd watchdog/notify
and bus variables are forbidden. Equal selections reuse the exact resolution.
At most 128 dynamic selections and 1 MiB of aggregate policy/resolved data remain
until connection retirement. Preparation accepts only an exact resolved program
and environment pair from this connection. The product owner checks Use and any
credential-export grant before invoking this mechanical capability.
