---
name: Session commands and preset selection retain their actual mutation owner
---

## Problem

An extension command needs the same pinned definitions and complete state as
execution. A client disconnect must not turn an admitted mutation into permission
to repeat its callback. Retaining only a draft pin loses edited initial values,
and a separate cached Header becomes stale after selecting another preset.

## Decision

The Agent owns command descriptors, typed invocations and compact receipts.
Commands enter the ordinary contribution registrar and immutable generation.
The [composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns effect-free callbacks, actual draft values, opaque draft identity and
move-only preparation results. Its receipt cache rejects changed input under an
existing request ID without evicting earlier identities to make room.

The [Turn contract](../../../../crates/rsi-agent/turn-protocol/README.md) owns the
separate Kernel-issued SessionCommands capability. Durable controls bind the
complete invocation, including its control predecessor, to the canonical domain
request. Exact retries query the original receipt before resolving a callback.
Prepared commands compete once at mutation admission; a losing revision does not
rerun against newer state. The preparation deadline includes capture, callback
and final admission. Once Store commit starts, the owner retains it through
acknowledgement reconciliation. Unknown external-command results remain queryable
without manufacturing an executing Turn failure.

The native [Session contract](../../../../crates/rsi/session/README.md) owns draft
leases and their admitted tasks. Exact concurrent command requests join one
task. Preset preparation also runs outside mutation admission, then competes
with commands and first publication at the same draft revision. A successful
selection replaces Header, pin and defaults together. Failure preserves them;
publication or expiry rejects late results. First submission freezes the actual
baseline under that admission.

The [Session API](../../../../crates/rsi/session-api/README.md) owns request-bound
wire validation. Creation version 2 returns the original creation input and the
current draft snapshot, preserving lease retry semantics after preset selection.
Each call and stream captures one Header binding. Selection validates the exact
successor before atomically updating the client binding; delayed replies cannot
regress it. Other handles refresh through attach. Read projections and application
commands consume these public seams without gaining Store write authority.
The shared Client owns one-send execution and query-only reconciliation. An
absent receipt may name an active callback, so applications keep the full
invocation until a matching receipt or a definite execution rejection. A failed
refresh, retired controller or changed pane never authorizes a new invocation.
Registered slash names take precedence over direct skill names; other slash
inputs remain Human messages for the workspace resolver.

## Alternatives considered

Reconstructing a baseline from a retained pin discards actual draft edits. Running
callbacks under mutation admission prevents independent work and publication
from making progress. Automatically repeating callbacks after a CAS conflict or
lost acknowledgement changes a logical invocation into a different operation.
Putting complete states in client receipts duplicates authoritative payloads.

Updating a remote Header separately from its fingerprint permits mixed bindings.
Checking every delayed reply against the latest mutable binding rejects valid
already-admitted work. Keeping the original creation preset as a permanent reply
invariant prevents rejoining a draft after a successful preset switch.

## Consequences

The [session protocol](../../../../crates/rsi-agent/session-protocol/README.md)
and [SQLite Store](../../../../crates/rsi-agent/store-sqlite/README.md) own the
current format versions. Unsupported databases are preserved and rejected.
Draft receipts remain process-local and end with publication or lease expiry;
they do not become a second durable command log. Independent replay of an old
Draft invocation against a durable Session fails its typed predecessor check.

Kernel tests exercise real Memory and SQLite stores, callback coalescing,
deadline, capacity, lost acknowledgements and cold queries. Native Session tests
exercise reconnect, actual baseline publication, expiry, retirement, concurrent
commands, preset preparation races and disconnected callers. API tests exercise
identity substitution, invocation hashes, revision errors, creation retries and
out-of-order Header bindings. Client and application tests cover retained
invocations, original-digest validation, bounded admission, generation replacement
and command-only execution without a model request. The separate
[projection decision](2026-09-09-session-extension-projections.md) owns derived
read state; ordinary [plan policy](../feature/2026-09-09-plan-policy.md) and
[repeat advice](../feature/2026-09-09-repeat-tool-reminder.md) consume the frozen
composition contracts.
