# rsi-meta-profile

`rsi-meta-profile` is the only owner of executable Profile composition. It is
an ordinary `rsi-meta` plugin constructed directly by `rsi-host`; it is not a
Runtime registry, catalog service, or second lifecycle engine. Custom
embedders may use Meta without it.

## Program and sources

A Profile starts from an empty tree and executes one ordered program:
immutable linked fragments, the selected root file and its includes, then
immutable launch patches. Every document has `format = 1` and an ordered
`[[steps]]` stream whose kinds are `include`, `group`, `plugin`, or `patch`.
Includes are required, resolve relative to their declaring file or from an
absolute path, and execute inline. The selected object is opened without
following its final symlink, must be the same regular file before and after
open, and is read through a byte bound; special files are rejected without a
blocking read. Canonical identities are checked against fixed cycle, include
depth, group depth, file-count, and aggregate-byte bounds.

Nodes are declarative groups or plugin leaves. `InstanceId` is unique across
the complete tree; one `PluginId` may appear at several leaves. Groups own
enabled state and exact Local, event, and Portable isolation declarations for
their descendants. Reparenting or changing a group's isolation retires and
recreates those descendants.

A patch either appends nodes to a group, replaces a leaf's entire config,
changes enabled state with group cascading, or replaces a group's complete
isolation declaration. Target absence and kind mismatch reject the candidate.
There is no remove, move, ID/plugin/source replacement, generic node replace,
deep merge, or missing-target skip.

TOML configuration accepts only the JSON-compatible subset and rejects datetime
values. Literal `config` and `config_rhai` are mutually exclusive, as are
literal `enabled` and `enabled_rhai`. Rhai computes only one complete config or
enabled value. Evaluation is bounded and pure: scripts can read the frozen
`paths`, `platform`, and `defines` values, but receive no Context, service,
environment, filesystem, network, or process access. Operation, expression,
and function-call depths plus string, array, and map sizes are pinned to
Profile limits. The operation ceiling is shared by every expression in one
rebuild rather than resetting for each program step. Scripts and configuration
values never appear in diagnostics.

## Preflight and convergence

Every startup or reload rereads immutable sources and rebuilds the candidate
from an empty tree. Parse, source bounds, expression evaluation, identity and
patch checks, factory resolution, context/isolation derivation, and watcher
capture complete before Runtime mutation begins. Equality and restart checks
therefore reserve no duplicate Fibers. Replayable convergence prepares and
applies only the next leaf after prior capacity has been released; rollback
does the same for the previous suffix. This keeps reload possible at the exact
Runtime Fiber ceiling instead of requiring a shadow copy of either graph. A
prepared leaf may commit as Pending when its declared dependencies are absent.

Equal healthy trees return `Unchanged` without advancing revision. Degraded
state never suppresses a same-content retry. A changed `RestartRequired` leaf
publishes the candidate source digest and `RestartRequired` status without
changing the observed graph. Replayable changes converge in the existing Meta
graph. Failure retires candidate generations and reconstructs the prior target;
failed compensation publishes `Degraded` with the observed graph and remains
retryable. During convergence the controller mirrors each membership delta
internally but publishes the complete observed graph only at attempt
boundaries, avoiding a full graph clone after every leaf. This is bounded
convergence, not atomic shadow-Runtime replacement.

## Static generations

`ProfileGenerationPlan` is the opaque, one-shot path for mounting one compiled
Profile candidate below a caller-owned Context without installing Profile
control or source watching. Construction resolves every enabled plugin against
the caller's immutable `ProfileResolver` before Runtime mutation. Activation
prepares every leaf against the supplied Context's Runtime before creating a
Fiber, then creates one ordinary wrapper Fiber, derives the same group isolation
as the controlled Profile path, and mounts every prepared leaf below the
retained wrapper Context. Preparation failure releases every admitted proof
without wrapper mutation. The method returns the wrapper handle only when every
leaf is Active; activation failure, a Pending leaf, or cooperative cancellation
disposes the wrapper and joins child rollback before returning failure.
Cancellation that intersects an in-flight blocking preparation waits for that
preparation to exit so its proof is released before the method returns.

A plan exposes only its source digest and required source paths. It does not
expose resolved factories, bound Contexts, or the resolver and does not turn the
resolver into a Runtime catalog service. Source changes never mutate a mounted
static generation: a product compiles and activates a new plan, then decides
when its own consumers may observe that generation. The wrapper factory rejects
Meta reconfiguration so it cannot activate again as an empty generation.

## Watching and control

The Profile Fiber watches the root and every transitive include. Change signals
use a serialized single-flight worker with a dirty bit, so a signal arriving
during reload causes one subsequent rebuild. A candidate watch plan is fully
established before mutation and replaces the old plan only after commit.
The portable polling watcher performs bounded metadata probes at its short
interval, rereads immediately when metadata changes, and forces a complete
content hash at least every five seconds to detect changes that preserve size
and modification time. Watcher read failures publish bounded redacted status
and retry only while the probed watch plan is still current. Any reload failure
before Runtime mutation publishes its bounded diagnostic and automatically
retries the unchanged failing source with capped backoff; manual reload remains
immediate. After candidate activation fails but rollback succeeds, the watcher
tracks the candidate source snapshot and does not reapply it until a watched
source changes or manual reload is requested.

The Fiber supplies typed Local `ProfileControl` with `reload`, `status`,
redacted tree `snapshot`, and last-value `subscribe`. Status bounds diagnostics
and source paths; it never contains configuration, Rhai source, or secrets.
Observed child state is a Profile-owned lifecycle category: `Failed` deliberately
does not carry the Runtime/plugin failure string, while `Pending` retains only
Meta's already-bounded dependency report.
The controller installs child watch receivers synchronously and refreshes the
current snapshots before awaiting them, so a transition between graph capture
and task polling cannot be lost. Subscriptions stay installed across child
state changes and are rebuilt only when graph membership changes. Every diagnostic source, including settled
child failure and watcher failure, passes through the configured retained-byte
bound.
Frozen `defines` visible to Rhai use the exact JSON subset of null, booleans,
strings, arrays, objects, and signed 64-bit integers. Floating-point numbers,
negative zero, exponent-form numbers, and integers outside `i64` are rejected
before expression evaluation; conversion never substitutes Rhai `()`.
Cross-process sockets, signals, CLI control, and native artifact watching are
outside this contract.

`source_digest` is a tagged digest of the complete frozen program, selected
source identities and bytes, and every environment path/platform/define input;
it never depends on Rust `Debug` formatting. Because native path bytes and the
explicit platform value are inputs, the digest is a host-platform-scoped
identity and must not be used as a cross-platform cache key.
