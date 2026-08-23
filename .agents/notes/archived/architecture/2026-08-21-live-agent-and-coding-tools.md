---
name: Live agent and coding tools ownership
comment: Ownership boundaries for the RSI Agent v1 runtime and production coding tools
---

## Problem

The first `rsi-agent` slice coupled a session identity to one bounded turn. It could not preserve useful state across caller turns, switch the selected model for later work, compact an active model surface, or own background commands and terminals. Extending that interface would have forced callers to coordinate durable state, recovery, and provider ownership outside `AgentHost`.

The earlier tools wire returned arbitrary JSON and had no owner epoch, rich image result, result acknowledgement, cancellation, or notification contract. Persistent jobs and PTYs need all of those boundaries because their lifetime exceeds one invocation.

This decision supersedes the first slice recorded by the archived Replayable agent turn runtime note while retaining its separation between `rsi-agent`, `rsi-meta`, and provider-neutral protocols.

## Decision

`AgentHost` remains the sole public runtime façade and owns live sequential multi-turn agents. It persists caller-owned turn identities, the immutable execution directory, current route and model, one context snapshot per model step, active-surface checkpoints, dispatch barriers, stable replay boundaries, notifications, and owner-epoch loss. Storage, scheduling, adapters, compaction, and stream coordination remain private.

Closing an agent is a recoverable detach, not a permanent durable tombstone. A
host restart likewise exposes saved sessions as `Detached`; a missing saved
language route is `Unroutable` until the caller selects a route available in the
current composition. Creating a session initializes its tools owner before the
session becomes publicly observable. Each model step opens one generation-pinned
language stream, obtains its provider-I/O-free `LanguageProfile`, prepares, and
dispatches on that stream; the next step is the first point that may observe an
HMR generation change.

Language capacity is configuration, not an adapter-family guess. Production
language plugin configuration names every admitted model and its exact context
window, default output reserve, and maximum output reserve. Both describe and
prepare reject model identifiers missing from that generation-pinned map, so a
durable context snapshot never records a generic constant as a per-model fact.

`rsi-agent-protocol` version 1 owns only provider-neutral owner, catalog, invoke, cancel, notification, rich-result, RAT1 blob, and result-acknowledgement semantics. Creating an agent binds the static tool-policy fingerprint; resuming requires the same fingerprint and sends it to the new tools owner. Provider-neutral language history records whether each tool call used function or freeform syntax when the call was emitted, so an adapter never infers historical wire syntax from a later catalog. The default and interactive coding tools live in one independently built production plugin under `plugins/rsi-agent/coding-tools`. `rsi-meta` continues to own composition routing, stream credit, plugin generations, and lifecycle without agent state.

Every accepted direct AI operation, manual compaction call, and Realtime session
remains host-owned until it reaches a terminal state. Drain waits for those
owners before stopping the durable writer; Cancel signals them and then waits.
Per-session lifecycle serialization fences resume, compaction, and close without
retaining one permanent lock allocation per historical session. Idempotent
create compares the immutable initial model selection, not a later mutable
selection.

Coding-tools opens one trusted Bubblewrap executable and executes that pinned
identity for commands and PTYs. Workspace image reads and patch-source deletion
operate through already verified directory or file handles. Owner teardown is
successful only after child processes and capture tasks are reaped; enforcement
failures propagate to the owning lifecycle instead of being discarded.

Provider-neutral DTO decoding enforces the same semantic relationships as its
constructors, including nonempty media subtypes and ordered model-capacity
limits. Aggregate tool bounds measure encoded JSON bytes. Responses projection
uses the declared function/custom kind for a forced tool choice and preserves
the top-level fields of a streamed provider error event.

The raw transcript is append-only and has no product session or database quota. Client-side compaction atomically replaces a separately bounded active model projection while retaining every raw event. Individual prompts, events, frames, requests, results, summaries, artifacts, jobs, terminals, and spills remain bounded.

Model requests persist their exact source boundary, model, and canonical digest,
not a second copy of the canonical request. The core rederives the bytes after
the commit and compares the snapshot before provider preparation. This keeps
the 1,600 KiB event boundary independent of the 16 MiB active request surface.
Successful compaction commits an audit-only provenance event and the adjacent
complete provider-neutral replacement node in one transaction; neither a
derived active-surface row nor a summary without its replacement can become a
second recovery authority.

## Alternatives considered

A public `Agent` handle, store trait, execution-environment trait, or tool-provider trait would expose coordination seams without a second production implementation. Human-approval suspension would add another state machine before Direct Mode has end-to-end evidence, so v1 uses a static deployment policy that is absent from model schemas.

Provider-native compaction cannot be the only path because recovery and route switching must remain provider-neutral. Per-tool crates would split jobs, terminals, output retention, patch interception, and cleanup invariants across artificial package boundaries. One coding-tools module keeps those resources under one owner actor and one policy.

Fixed transcript quotas would eventually terminate a healthy long-running agent after compaction. Automatic deletion or archival would create an integrity and lifecycle policy that the product does not yet have. The append-only design leaves disk capacity to deployment operations and fails closed when a durable transition cannot be committed.

A persisted machine checkpoint could bound cold reconstruction, but it would add another durable authority whose atomicity and integrity must remain consistent with the append-only event log. V1 instead replays and validates that log when reconstructing a session. Notification-index validation is batched and one operation reuses one SQLite snapshot, but cold reconstruction remains linear in that session's retained event count.

## Consequences

The v1 surface supports create, resume, turn, model selection, cancellation, manual and automatic compaction, paged reads, close, shutdown, notifications, and direct AI operations through `AgentHost`. Schema v8 rejects v1 through v7 without migration. Recovery never repeats an unknown external effect.

The production plugin exposes the exact default names `bash`, `job_output`,
`job_list`, `job_cancel`, and `apply_patch`; `view_image` is added only when the
captured step profile explicitly accepts image tool results. Interactive mode
adds `terminal_open`, `terminal_send`, `terminal_read`, `terminal_signal`,
`terminal_close`, and `terminal_list`. It registers no aliases. Static config
owns the `default|interactive` preset, kebab-cased sandbox mode, disabled
network, environment allowlist, and completion delivery.

The tools stream replenishes input credit after consuming each DATA request.
Uncommitted invocation-result slots are owner-bounded, and one maximum image
sequence, every admitted result/credit suffix, and one completion notification
for every retained job may wait in the bounded outbound queue while per-frame
credit paces publication. A temporary credit or host-queue shortage therefore
defers the remaining frames without emitting an incomplete blob.

Native image bytes retained for an uncommitted result are separately bounded to
one maximum 32 MiB image per owner. Result acknowledgement releases both the
invocation slot and its exact retained-byte charge; admitting another image
before that acknowledgement fails with a model-visible resource limit instead
of growing owner memory.

A background terminal send reserves its settled virtual job before delivering
input and removes that reservation if delivery fails. Job retention advances
only when the acknowledged result actually contained a terminal job state; a
committed running observation cannot make a later settled state evictable.

Patch publication returns a non-model-visible digest `AppliedPatchDelta` in
actual commit order. A later publication failure retains the committed prefix;
in particular, a move whose destination write succeeds before source removal
fails is recorded as that destination add. The protocol permits this private
provenance beside either a successful or failed semantic result.

On Linux patch staging, publication, and source removal walk parent components
relative to the pinned workspace directory handle with no-follow opens. Staged
files, final renames, and unlink operations remain relative to those verified
parent handles, so replacing an intermediate directory with a symlink between
preflight and publication cannot redirect a mutation outside the workspace.

On Linux every child sandbox mode requires Bubblewrap. Full-filesystem mode
still unshares the network because `network=false` is invariant configuration,
not an implication of the filesystem mode. Startup opens one root-owned,
non-group/other-writable Bubblewrap executable, probes that file identity, and
invokes the same still-open handle for commands and PTYs. They apply their fixed
`TERM` values after copying allowlisted environment variables.

Removing transcript and database quotas allows one workspace to consume available disk. Disk-full and uncertain SQLite commits therefore make the host terminal, and an already dispatched effect may remain `OutcomeUnknown`. There is no automatic deletion or in-place repair interface.

Active-surface compaction bounds the model-visible projection, not the cost of
validating raw history. Cold recovery fixes a SQLite read-snapshot head and
folds the raw log from sequence one in pages of 256 without constructing a full
history vector. A long-lived session therefore trades linear cold-read latency,
but not linear retained-event memory, for complete evidence and one
authoritative replay source. Exact call-ID and notification indexes are derived
read models and never replace that fold.

The provider-neutral core has no tokenizer shared by every language route. Its
compaction pressure and recent-suffix calculations therefore use a deterministic
canonical UTF-8 byte count divided by four, taking the larger of that estimate
and reported usage when the latter exists. This keeps replay stable across route
changes but makes the percentage a conservative approximation rather than a
provider-token-exact promise.

PTY behavior, process groups, and sandbox backends are platform-specific. Linux evidence does not establish native macOS or Windows behavior. The implementation and verification reports must name only the platforms they exercised.

The product conformance gate owns the standalone production coding-tools
workspace as well as the fixture workspace: each is formatted, checked with
warnings denied, and tested before the assembled scenario. Because the shipped
coding-tools artifact and Bubblewrap backend are Linux x86_64 only, that native
gate runs only on the matching CI platform rather than implying a macOS result.
