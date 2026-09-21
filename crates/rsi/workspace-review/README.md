# rsi-workspace-review

WorkspaceReview is an ordinary product plugin implementing the Executor's optional
execution observer. It records changes observed between two filesystem captures,
not causal attribution to a model or Tool. Native intervals identify Session,
Turn, claim, accepted/live Fact cut and runtime epoch. A recovered claim beyond
initial acceptance cannot reconstruct the original Turn baseline and is marked
partial. Unconfirmed controlled-work settlement is never labeled complete;
unmanaged or external-peer background writers are outside that proof.

Captures enumerate tracked and nonignored untracked paths below the selected Git
workspace. Existing dirty bytes are part of the baseline. Git executes only through
Process and Sandbox with a fixed environment, disabled hooks/config helpers and no
external filters. Listing reads the user's index; snapshots write a separate private
index and object database. No user index, refs, objects or worktree writes are
performed. Files reads use generation-bound, no-follow workspace authority and
reject files that change during reading. Nested repositories, symlinks, binary or
invalid UTF-8 text, unreadable paths, ignored paths and non-Git workspaces have
explicit coverage limitations. Captures are not atomic filesystem snapshots.
Git also lists tracked paths absent from the worktree. A Files open reporting
absence records no file in that capture, so ordinary deletions remain comparable.
Other open failures are explicit unreadable omissions.
Interval completion fences late baseline tasks: a finished interval cannot become
active again or acquire new scratch.

One capture admits at most 10,000 paths, 1 MiB of path names, 4 MiB per file and
128 MiB of original bytes. There are two retained capture workers. The at-most-eight admitted intervals
wait for those workers within their cancellation deadline; API reads use a
separate two-slot pool and cannot consume baseline capture capacity. Each interval has at most 512 MiB of private scratch; the owner admits at
most eight intervals retaining scratch and 1 GiB total scratch reservation.
Capture stages expire after 30 seconds and preserve omission reasons. Drops and
retirement cancel subsequent steps and drain admitted Process/Files/storage work.

Summary Domain `rsi.workspace-review`, version 1, stores at most 8,192 summaries,
256 KiB each and 64 MiB total. It lives outside the Agent Store. Pending summaries
are durable before capture; a lost acknowledgement closes further write admission
until restart and cannot be claimed as success. A list carries the current epoch
so old unfinished captures can be shown as interrupted without inventing an end
time or rewriting historical evidence.
Current-runtime diffs live only in private scratch and carry the exact runtime
epoch. Restart leaves summaries readable and returns `Expired` for old diffs.
Capacity exhaustion never silently removes an old durable summary or claims that
an omitted capture was complete. The [API](../workspace-review-api/README.md)
owns bounded reads and source authorization.

Capture imports validated in-memory file bytes with Git fast-import in batches
of at most 64 files and 8 MiB of original bytes. Private blob marks never name
worktree paths or refs; no user filters or index writes are involved. Repository
ignore files apply to listing; ambient global Git configuration is intentionally
excluded from the fixed execution environment.

Each retained comparison caches its most recently requested file patch (at most
4 MiB) so adjacent 64 KiB pages share one immutable Git result. Switching files
replaces that patch; eviction or retirement releases it with the private scratch.
