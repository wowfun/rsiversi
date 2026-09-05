---
name: Durable workspace trust for instructions and skills
comment: Prevent project-controlled context from becoming ambient authority
---

## Problem

This note owns the trust decision and user-visible workspace-context policy;
the generic durable scheduling and Fact lifecycle remain in the
[Session Kernel note](2026-08-26-durable-session-kernel.md). The current package
contract is documented by
[Workspace Context](../../../../crates/rsi-agent/workspace-context/README.md).

Workspace instructions and skills alter model-visible context and can influence
Tool use. Loading project files for every Session would let an arbitrary cloned
repository inject instructions or shadow a trusted user skill. Re-evaluating
trust from current process state on resume would also let one durable Session
change meaning across Host generations or forks.

Filesystem context is mutable after Session creation, so the Agent needs both a
stable authority decision and durable evidence of the exact snapshots supplied
to each Step. The Kernel must remain the sole writer of model-visible Facts
rather than delegating durability to the filesystem adapter.

## Decision

`WorkspaceTrust` is an explicit immutable field in the durable Session Header and
defaults to `untrusted`. A fork preserves the parent's trust decision together
with its canonical workspace. Changing trust requires creating a new Session;
attach, resume, executor replacement, and Host restart do not reinterpret it.

`WorkspaceContext` is a process-local interface that returns one bounded
observation for a validated Header and the current messages. Missing, malformed,
oversized, or Session-unsafe optional sources are complete omissions. Unexpected
filesystem I/O marks the observation incomplete, causing the Kernel to preserve
last-good durable context instead of publishing partial replacement or tombstone
Facts. Configured user
instruction and skill roots are trusted and remain eligible in both modes. An
untrusted workspace contributes no project `AGENTS.md` and no project skills. A
trusted workspace discovers the nearest `.git` ancestor, reads instructions from
that root down to the Session cwd, and scans only the root-level
`.agents/skills` directory.

User roots are evaluated before the project root, and first selection wins, so a
project skill cannot shadow an identically named user skill. Skill metadata
separately controls model visibility and direct-user invocation. Only a direct
Human message may request a selected user-invocable skill body; Agent messages,
completion messages, Tool output, and model output cannot manufacture that
authority.

The Kernel refreshes the complete snapshot before every provider request and
writes instruction replacement, skill-catalog replacement, invocation, and
tombstone Facts. Content digests suppress unchanged replacements, while an empty
later snapshot removes an earlier nonempty view. Source counts, paths, entries,
individual files, rendered text, and the final Fact batch are all bounded before
entering trusted runtime state.
The rendered-byte ceiling deterministically prioritizes configured user
sections and the deepest project sections, then emits selected project policy in
root-to-cwd order; a skill catalog keeps its lexical prefix. `complete` describes
a coherent bounded observation, not inclusion of every eligible source byte,
and each digest names the exact model-visible bounded result.
The Store maintains an offline-verifiable digest projection from canonical
workspace input Facts, so cold recovery restores suppression state without
republishing an unchanged baseline.
The trusted project root is opened once as a directory capability, and every
project source is resolved relative to that handle. Concurrent ambient renames
or intermediate symlink replacement therefore cannot redirect reads outside
the selected authority. Sources are streamed only through their configured byte
limit, unsafe NUL/DEL text is omitted, and rendered limits count UTF-8 bytes
without splitting a scalar. Skill enumeration reads only the remaining global
allowance plus one overflow probe, sorts the retained prefix, and marks an
overflowed observation incomplete. Catalog discovery reads only a 16 KiB metadata
prefix; the bounded body is reopened only when a direct Human invocation selects
it. That second read must reproduce the selected name, normalized description,
and invocation flags before its body is attributed to the catalog identity. A
concurrent identity change makes the observation incomplete so the Kernel keeps
last-good workspace context rather than durably mislabeling the new body.

## Alternatives considered

Unconditionally loading project instructions and skills was rejected because
repository contents are not user authority. Allowing project skills to override
user skills was rejected because a checkout could silently replace a trusted
name. Treating system configuration as another filesystem precedence layer was
rejected because user and project discovery should not override system or direct
user instructions.

Recomputing trust on attach or inheriting the current parent's process-local
choice at each fork was rejected because durable history would no longer have one
stable interpretation. Allowing any message source to invoke a user-invocable
skill was rejected because Agent-controlled text could impersonate a direct
Human request.

Persisting raw filesystem paths as the authority was rejected. Paths identify
sources for evidence, but the immutable Header trust decision and durable
replacement Facts determine what the model actually received.

## Consequences

Opening an existing Session in a newly trusted checkout does not upgrade it; a
new Session is required. A trusted Session observes later project edits before
the next provider request and records replacements or tombstones, so its context
can evolve without silently rewriting prior Steps. An untrusted Session can still
use explicitly configured user instructions and skills.

Malformed, oversized, unsafe, or out-of-scope filesystem entries are omitted
from a complete bounded observation. Unexpected read failures make it
incomplete, retaining last-good durable authority. The current project
definition is the nearest ancestor containing `.git`; supporting
other workspace authorities requires an explicit interface change rather than
filesystem heuristics in callers.
