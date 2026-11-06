---
name: One application owner assigns plugin Session event identities
---

## Problem

Rebuilding every DSH event position from native Fact and control order cannot
implement a plugin's synchronous Session append. DSH returns the accepted
immutable event immediately; persistence is asynchronous. A queued native command
cannot return its future projected position while execution can append more Facts.
The original Better Sidebar model route uses this contract directly. Rejecting
that local append prevents an ordinary model selection without protecting a
separate authority boundary.

The [mirror decision](2026-11-03-application-session-mirrors.md) rejects a competing
execution history. This decision partially supersedes its assumption that every
DSH position comes solely from native order. An application presentation log and
native execution history serve different owners.

## Decision

One application Session log assigns dense event positions and synchronously
accepts immutable model-selection and inbox events. Attached Agents, workbench history and
cold persistence use that owner. Kernel remains the only execution owner. Native
observations enter the log through a retained source-to-application position map.
Every source reference is translated through the map. The native counterpart of
a locally accepted event normally aliases its existing position.

Aliasing alone cannot represent an intervening external model choice followed
by the local command taking effect. In that case the owner publishes a new
reconciliation event with the effective choice. The original accepted event never
moves. An older acknowledgement cannot displace a newer pending local selection.
The original DSH model-selection fold consumes the ordinary resulting events.

Inbox projection also distinguishes accepted application order from native queue
order. Native controls retain their input identities while their positional
splices are translated into application coordinates. Exact command provenance
and the original message envelope correlate a local insertion with its native
counterpart. Presentation IDs may be reused after removal; they cannot identify
which acceptance a delayed native claim consumes. The journal hashes the raw
source and verifies the translated body separately. Cold replay uses the stored
application prefix at each source position, not the final inbox state.

The application persists its locally owned events, the source map and typed
mutation identities together. Native event bodies remain in Store and are rebuilt
at their mapped positions on cold loading. Inbox reconstruction uses the same
prefix fold as live observation; a generic JSON patch format would introduce a
second interpreter for those semantics. Local acceptances, including their exact
timestamp and original queue target, remain durable after acknowledgement.
Before dispatch, the application records and flushes the exact native invocation.
Recovery reuses that invocation and request identity; it never silently restamps
a revision or invokes under a fresh identity. A failed attempt does not erase an
accepted event. The latest unresolved choice blocks dependent Agent submissions;
an explicit later selection can supersede it. The current native transport does
not distinguish rejection from an unknown outcome, so retained diagnostics do
not claim a definitive rejection. Application persistence failure and native
publication remain distinct outcomes.

The external bridge owns this log and private profile storage. No DSH event codec
enters Meta, Host or Kernel. Native-only mirrors need no application file. Once a
local event exists, the application log is durable state, not a disposable cache.
The single application process owns its writer; Host retires it before starting
a replacement. Retained readers share a projector that processes only new native
observations. Each caller supplies its own authorized read; sharing content grants
no request authority. Native projection batches publish atomically.

Inbox reads follow DSH's retained projection cursor: immutable event prefixes do
not need copying or refolding when more events arrive. Mutable operation metadata
has a different lifetime. The journal indexes operation identities by accepted
event position, and pending entries read their current preparation and receipt
through that index. This keeps late native identities visible without discarding
the fold. Cold validation and transactional native-observation publication remain
at their existing boundaries; a failed splice cannot advance the retained cursor.

The owning contract lives in the external bridge's `docs/session-journal.md`.
Native application authority remains documented by [Serve](../../../../apps/serve/README.md).

Lineage queries combine visible native headers and watermarks with compact
application journal metadata. Reconstructing each conversation to discover
its parent would make an export pay for every Session's history. Keeping the
application event count beside the stored header also lets metadata readers
observe plugin appends between native commits.
History reconstruction uses bounded Fact pages within a captured horizon and
borrows referenced evidence from the page already being folded. The normal page
byte bound does not force a large individual Fact into an unrepresentable log:
the existing fragmented transport handles that record alone. Warm durable mirrors
compare indexed cursors before opening full model projections. These changes
reduce IPC and repeated work without another durable event corpus or an unbounded
body cache. The exact application interface is owned by Serve's contract.

## Alternatives considered

A predicted sequence breaks immutable identity. A Promise or missing event breaks
the synchronous API. Blocking the shared Worker on native I/O stalls every browser
and plugin callback. Moving execution into JavaScript creates a second scheduler.
Separate application logs for live and cold readers give the same Session different
cursors. Rejecting all synchronous plugin writes avoids implementation work but
breaks compatible local UX without establishing an additional trust boundary.

DSH's JSONL provider persists its owned Session event stream. In RSI, copying the
complete native-derived stream into every application snapshot instead duplicates
already durable message and tool bodies. Sparse application records retain only
the information this owner contributes. A current full-log file is rewritten after
successful native reconciliation, preserving existing plugin state without ongoing
dual writes. A failed reconciliation must not rewrite it during owner disposal.

## Consequences

Original Side Chat model selection now runs unchanged. The application accepts
the event synchronously and the surrounding publication waits for the native
receipt. This does not make native execution or disk persistence synchronous.
Model selection, text input and inbox removal integrate with native execution.
The public Agent immediately exposes accepted input through the same event fold
used by workbench readers. Generated native command IDs are independent of the
original message ID. Removal records its intent before returning and never
retracts that event on I/O failure. Each removed acceptance settles independently;
clear preserves next-step then next-turn order. Native held draft inputs are
observations, not commands to replay after a process restart.

A command removed before preparation is never dispatched. Once a command's
live owner has prepared an invocation, it retains knowledge of non-dispatch
across a failed log write until an actual dispatch attempt. Treating that known
state as an unknown native outcome would unnecessarily prevent local withdrawal.
After restart, a prepared record alone does not supply that knowledge. Once its
outcome is unknown, withdrawal queries its receipt without re-executing it. The
native query can return no receipt while work is still pending; that answer
cannot justify either cancellation success or a new invocation. A native claim
rejects withdrawal truthfully, while a definitive rejection does not permanently
block later prompts. Structured command-outcome reporting remains needed to
distinguish terminal rejection from unknown outcome. Waking followup/steer input
now retains original attribution through native direct ingress, using independent
generated native IDs. The same journal accepts these messages synchronously and
drives exact submissions. A read-only native receipt plus the original accepted
body resolves a lost acknowledgement without replay merely to withdraw. Completion
retains the immutable acceptance cursor, not mutable claim state. Native idle
steering may route next-turn while the original DSH splice remains next-step;
correlation follows acceptance identity across queues rather than rewriting the
accepted event. Arbitrary execution-event appends remain separate work.

Tests cover stable identity, failed batches, stale readers, observer failure,
exact-invocation retry, interleaved choices, fixed captured intervals and cold
reconciliation against two published DSH cohorts. Live verification exercises the
original browser route, two independent browser contexts, the actual provider,
and a Host restart. These checks do not establish general DSH Agent compatibility.

The application needs its own durable lifecycle. Native and application cursors
must not be interchanged. Whole-document atomic snapshots retain local events and
mapping metadata; cold reconstruction shares the first native read batch but still
materializes the dense Session in memory. Large-history paging, incremental persistence and expired native
draft-file cleanup remain work. A native draft receipt is process-local and does
not justify recreating a lost draft after restart. These are implementation limits,
not reasons to impose more confirmation steps on ordinary local model selection.
