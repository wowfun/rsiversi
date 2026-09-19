---
name: Durable workspace observations for instructions and skills
comment: Keep mutable filesystem observations coherent and attributable
---

## Problem

Mutable filesystem context needs durable evidence of the exact instructions and
skills supplied to each Step. The Kernel must remain the sole writer of
model-visible Facts; filesystem adapters provide bounded observations.
The selected-source policy belongs to the
[default workspace decision](../simplification/2026-09-19-default-workspace-context.md),
and current limits and source rules belong to the
[Workspace Context contract](../../../../crates/rsi-agent/workspace-context/README.md).

## Decision

`WorkspaceContext` is a process-local interface that returns one bounded
observation for a validated Header and the current messages. Missing, malformed,
oversized, or Session-unsafe optional sources are complete omissions. Unexpected
filesystem I/O marks the observation incomplete, causing the Kernel to preserve
last-good durable context instead of publishing partial replacement or tombstone
Facts. Source discovery and precedence follow the selected workspace contract.

Skill metadata
separately controls model visibility and direct-user invocation. Only a direct
Human message may request a selected user-invocable skill body; Agent messages,
completion messages, Tool output, and model output cannot manufacture that
authority.

The contributor refreshes the complete snapshot before every provider request and
proposes instruction replacement, skill-catalog replacement, invocation, and
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
The project instruction root is opened as a directory capability. Skill roots
instead follow directory links and pin their resolved target for one observation.
Concurrent ambient renames cannot redirect a read away from its selected handle. Sources are streamed only through their configured byte
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

The original source-authority decision rejected unconditional project discovery
because repository contents are not user authority. It also stated: "Allowing
project skills to override user skills was rejected because a checkout could
silently replace a trusted name." Trust was frozen in the Header so attach and
fork could not reinterpret that choice. Those source-selection decisions are
superseded by the [selected workspace decision](../simplification/2026-09-19-default-workspace-context.md),
which explicitly accepts project-name shadowing and preserves its rationale and
risks. The durable observation and publication rules in this note remain current.
Treating system configuration as another filesystem precedence layer remains
rejected: filesystem context cannot override the authority of system or direct
Human instructions.

Publishing partial observations was rejected because a transient failure could
silently replace or erase valid instructions. Treating any message source as a
skill invocation was rejected because Agent-controlled text could impersonate
a direct Human request. Persisting paths alone was rejected: durable replacement
Facts, rather than later filesystem contents, determine what a model received.

## Consequences

All source generations share four blocking jobs with a conservative 16 MiB
aggregate envelope per job. The existing per-source, metadata, catalog and render
bounds remain in force. The new aggregate bound rejects excess invocation output
as Capacity instead of publishing a partial success. Four lanes and 64 MiB total
admission are explicit capacity policy rather than benchmark-derived tuning.
The actual blocking job and any unclaimed result own their permits; source
withdrawal closes admission, requests cooperative cancellation and waits for real
jobs. An operating-system filesystem call already running cannot be forcibly
cancelled, so it occupies capacity until it returns.

Every selected workspace observes later project edits before the next provider
request and records replacements or tombstones, without rewriting prior Steps.
An incomplete observation retains the last-good baseline and invocation cursor.
A bounded diagnostic is entered once per failure episode, including before the
first complete observation, so project failures cannot silently suppress user
context indefinitely. Complete recovery clears only the diagnostic latch.

Malformed, oversized, unsafe, or out-of-scope filesystem entries are omitted
from a complete bounded observation. Unexpected read failures make it
incomplete, retaining last-good durable authority. The current project
definition is the nearest ancestor containing `.git`; supporting
other workspace authorities requires an explicit interface change rather than
filesystem heuristics in callers.
