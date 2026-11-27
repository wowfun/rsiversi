# rsi-workspace-review

WorkspaceReview is an ordinary product plugin implementing the Executor's optional
execution observer. It records changes observed between two filesystem captures,
not causal attribution to a model or Tool. Native intervals identify Session,
Turn, claim, accepted/live Fact cut. A recovered claim beyond
initial acceptance cannot reconstruct the original Turn baseline and is marked
partial. Unconfirmed controlled-work settlement is never labeled complete;
unmanaged or external-peer background writers are outside that proof.

Comparison paths are sorted once at capture. Exact Diff lookup uses that ordering
before the one-entry patch cache, without rescanning the whole comparison.

Captures enumerate tracked and nonignored untracked paths below the selected Git
workspace through the original claim execution lease. Missing remote authority or
a mismatched location is rejected before capture admission. The interval retains
its admitted operation through completion, including independently retained
capture tasks. Each new capture
rechecks that lease; reconnect cannot replace its provider. Both captures list the
source before reserving space for a private Git repository. An empty inventory
is a valid baseline for files added later and needs no file reader. Failed listings retain
their omission reason. The source is a bounded
repo bundle of validated relative names, versioned bytes and executable flags.
No target Git objects, refs, config or index are imported into Local scratch. Existing dirty bytes are part of the baseline. Git executes only through
Process and Sandbox with a fixed environment, disabled hooks/config helpers and no
external filters. Listing reads the user's index; snapshots write a separate private
index and object database. No user index, refs, objects or worktree writes are
performed. Git inventory explicitly uses the ReadOnly source-reader view so repository
ancestors remain visible. Files reads use the same lease's generation-bound,
no-follow workspace authority and
reject files that change during reading. Nested repositories, symlinks, binary or
invalid UTF-8 text, unreadable paths, ignored paths and non-Git workspaces have
explicit coverage limitations. Captures are not atomic filesystem snapshots.
Git also lists tracked paths absent from the worktree. A Files open reporting
absence records no file in that capture, so ordinary deletions remain comparable.
Other open failures are explicit unreadable omissions.
Interval completion fences late baseline tasks: a finished interval cannot become
active again or acquire new scratch.

One capture admits at most 10,000 paths, 1 MiB of path names, 4 MiB per file and
128 MiB of original bytes. Concurrent sessions keep independent baselines and
share two capture workers. They wait for a worker within their capture deadline.
API reads use a separate two-slot pool. Each interval can reserve up to 512 MiB
of private scratch within a shared 1 GiB budget. When a reservation needs space,
the oldest completed comparison with no readers releases its scratch first.
Current baselines and in-flight reads retain their files. If the available
scratch cannot cover a capture, its review reports the omission and execution
continues. Starting a chat consumes no completed comparison by itself.
Capture stages expire after 30 seconds and preserve omission reasons. Drops and
retirement cancel subsequent steps and drain admitted Process and Files operations.

An interval keeps its turn-start baseline private until the first Tool start,
rejection, or recovery of an outstanding Tool. That transition makes the
pending review visible in the running Host. The completed comparison is
published to history only if it found a changed file. An interval with no
observed changes releases its baseline and its list entry.

A completed native comparison records its Turn ID in the Session's control
history. The running Host owns its counts, coverage, file list and patches.
Evicting the comparison releases its listing and scratch together. After a
restart the review panel starts empty. The marker follows the Session through
retirement and purge. The [API](../workspace-review-api/README.md) owns bounded
reads and source authorization.

Comparison uses the captured object ID and file mode for each path known at
both ends. Omissions accompany partial comparisons that found changes.

Capture imports validated in-memory file bytes with Git fast-import in batches
of at most 64 files and 8 MiB of original bytes. Private blob marks never name
worktree paths or refs; no user filters or index writes are involved. Repository
ignore files apply to listing; ambient global Git configuration is intentionally
excluded from the fixed execution environment.

Each retained comparison caches its most recently requested file patch (at most
4 MiB) so adjacent 64 KiB pages share one immutable Git result. Switching files
replaces that patch; eviction or retirement releases it with the private scratch.

The scratch writer lease explicitly unlocks on release, including failed recovery
after lock acquisition. Its persistent lock file is never unlinked; duplicate
descriptors cannot extend the owner lifetime. Only lock contention is capacity.
