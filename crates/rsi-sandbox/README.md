# rsi-sandbox

`rsi-sandbox` defines file-effect sandbox modes, process plans, and truthful
enforcement stamps. [`rsi-sandbox-local`](local/README.md) is an ordinary plugin
that behaviorally probes explicit Linux Bubblewrap candidates first and
explicit Landlock runner candidates second. Standard composition supplies no
Landlock candidate.

The local factory has optional and required activation policies. Optional
activation may publish an unconfined service for holders that use only
`danger-full-access`; required activation fails before publication when no
restricted backend passes its behavior probe. The standard product selects the
required policy whenever it links effect-bearing coding Tools, so those Tools
cannot become ready with enforcement deferred until their first call.

Restricted process calls fail closed without a selected backend. The
`danger-full-access` mode is an explicit holder bypass and is stamped as
unconfined. This family builds process plans; the process owner remains
responsible for spawning, cancellation, output bounds, and recording the stamp
in Agent facts.

`confine_source_reader` is a separate pipe-only ReadOnly plan for trusted, fixed
source collectors. The Linux provider requires its verified Bubblewrap backend,
keeps the host scratch namespace visible read-only, and isolates networking.
This permits ancestor instructions and explicitly discovered directory links in
`/tmp` to remain visible. Its evidence is ReadOnly + Host scratch + Isolated
network. It rejects write modes, PTY intent, dangerous workspace roots and
unsupported backends. Ordinary `confine` plans retain their private scratch and
host network semantics. This interface does not infer a source-reader exception
from an arbitrary executable; the consumer owns selection of its fixed collector.

Workspace reads use a separate, process-local `WorkspaceReadScope`. The Sandbox
provider binds the exact mode, cwd, workspace and its opaque generation. All
three existing modes allow read-only access inside that workspace; the file
provider enforces relative directory-handle reads without following symlinks.
Issuing this scope neither opens files nor emits a process enforcement stamp.
Tool policy and approval still precede execution. Paths are preserved exactly
after native, bounded lexical validation, so a later filesystem symlink cannot
silently retarget the pinned workspace through canonicalization.

Explicit backend candidates are opened without following a final symlink,
must be regular files, and are copied from the pinned handle through a fixed
byte ceiling before any probe runs. FIFOs, devices, and oversized candidates
cannot block or fill staging storage.

`read-only` and `workspace-write` describe file writes, not secrecy or network
policy. Durable stamps identify the staged backend bytes by SHA-256 rather than
an ephemeral staging path and separately record filesystem, scratch, and
network evidence. A provider supplies its selected network explicitly when
constructing a stamp. Bubblewrap restricted plans use a private tmpfs `/tmp`; a
workspace whose live canonical path names the system temporary root is rejected
in either restricted mode because its later bind
would erase that boundary. Bubblewrap also rejects the logical or canonical
filesystem root because a later root rebind would erase its private `/tmp`, `/proc`, and `/dev` mounts,
while a workspace below `/tmp` is rebound after tmpfs creation. Ordinary plans retain host network access and never claim filesystem
confidentiality or network restriction.

Restricted plans do not impose memory, CPU, process-count, or scratch-size
quotas. Scratch tmpfs mounts use the backend's host-dependent defaults, and
processes inherit the process owner's environment. These modes protect the
stated file-write boundary; they do not protect host availability from resource
exhaustion or conceal inherited secrets. Resource isolation requires a separate
process/container policy at the deployment boundary.

PTY intent is explicit, typed and process-local. Ordinary pipe execution rejects
PTY plans. The Linux local PTY path requires a verified Bubblewrap plan in
read-only or workspace-write mode; it never falls back to Landlock, unconfined
execution, or a pipe. portable-pty establishes setsid and TIOCSCTTY before the
wrapper; only the PTY plan omits Bubblewrap's `--new-session`. Namespace,
`--die-with-parent`, filesystem and scratch boundaries remain in the plan.
PTY terminal sessions are live resources, not durable enforcement stamps.

Linux PTY plans pin the workspace directory when confined and carry that handle
into the child before Bubblewrap resolves its bind-fd source. Replacing the
pathname between confinement and child launch cannot substitute the launch
workspace. The Process owner retains opaque plan resources through child
settlement. This pin identifies the workspace at confinement, not the inode at
the time a Session was first saved; Session authorization remains a saved
canonical path. Pipe/Landlock plans retain their existing pathname contract.
PTY confinement rejects symlink traversal and fails closed if native directory
pinning is unavailable. It requires Linux openat2 and Bubblewrap bind-fd support.

Mount-internal rename safety still depends on the selected Bubblewrap backend.
Older bind-fd backports can resolve the descriptor to a path without checking the
mounted inode afterward. A successful plan or pre-launch replacement test does
not prove that stronger guarantee against concurrent host filesystem mutation.

The separate `confine_isolated` port supports Linux isolated process scopes. It
accepts issuer-owned service identities with a bounded `rsi-` namespace; the
consumer owns its suffix and uniqueness, independently of its product family. It
requires verified Bubblewrap, clears the child environment, unshares PID,
network and mount namespaces, binds fixed runtime resources read-only, and
creates private `/tmp`, `/proc` and `/dev`. It wraps the scope with a user systemd
unit: MemoryMax 1 GiB, TasksMax 256, RuntimeMaxSec 600, TimeoutStopSec 10,
KillMode control-group and UMask 0077. There is no Landlock or unconfined fallback.
Before sending initialization or untrusted work, the consumer calls
`verify_isolated_limits` on the launched scope. The Linux provider requires the
unified cgroup hierarchy, the active owned unit and its control-group kill/runtime
policy, and reads that unit's actual `memory.max` and `pids.max`. Missing or weaker
limits fail closed. A requested systemd property alone is insufficient evidence.
Process owns launch and reaping; Browser verifies the runtime and readiness and
owns its bounded egress broker. These are separate guarantees from ordinary
pipe/PTY plans. Native proof lives in Browser's explicit Linux acceptance tests.

Isolated read-only mounts cannot target `/tmp`, `/proc`, `/dev` or their
descendants; those complete subtrees belong to private scratch and kernel mounts.
These mounts are issuer-owned immutable pathnames, not hostile-host file
capabilities. The issuer keeps each resource and its ancestors unchanged from
plan validation through scope retirement. A concurrent host operator replacing
those paths is outside this contract; isolated plans do not carry bind-fd mounts
through the systemd user-service launch. Program containment uses path components.
