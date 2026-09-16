---
name: Frozen human-selected Session references
---

## Problem

Copying a changing conversation into a draft loses its origin and cannot support
an exact retry. A draft reference must become durable Agent message content while Context
remains pure and has no Store authority.

## Decision

Agent session-protocol owns ReferenceSnapshotRef, FrozenReference and the closed
CAS envelope. A product capture reads a bounded suffix of a durable source
Session and stores an immutable envelope bound to source and original target
Session IDs and Header fingerprints. The current source horizon and Fact-prefix
digest are captured atomically with that suffix. Only direct human text and
visible conversation assistant text are exported. Tools, reasoning, instructions,
skills and nested references are excluded. The draft freezes the returned value
before submission; retries reuse it. Admission verifies the immutable CAS value and
checks its binding and preview before copying it to AgentMessageContent.

The [Session API target fence](../../../../crates/rsi/session-api/README.md)
also constrains the capture source for contribution grants. A contribution that
knows another Session ID must not gain its conversation text through a grant to
the receiving Session. Full application connections own explicit cross-Session
capture; recorded references still carry the user's earlier selection into a
contribution's authorized Session.

Capture scans at most 1,024 Facts and 16 MiB of encoded Facts. The Store checks
body lengths before materialization and may return an empty suffix when its last
Fact exceeds the remaining byte budget. Reference content is at most 1 MiB;
preview is at most 8 KiB and counts against the existing human-text budget. A
message contains at most four references. Explicit metadata records the source
horizon, scanned and retained intervals and omission reasons. Empty exports fail.

The pure Context builder renders the preview as user data with an exact recorded
Session/Fact/content-index locator. reference_read resolves only that recorded
entry in the current Session, or the actual direct-parent interval inherited by
the current Header. It never accepts arbitrary CAS digests or recursively follows
ancestor lineage. Read pages contain at most 64 KiB. Unsubmitted snapshots follow
the Store's existing retain-all CAS policy; this feature introduces no collector.

The durable message variant changes Session format 13 to 14 and Store schema 20
to 21, with exact version refusal and no implicit migration or deletion. Context
builder version changes 2.4 to 2.5; checkpoint outer version 6 and fold version 7
remain unchanged because their existing builder identity invalidates stale data.

## Alternatives considered

Client summaries cannot establish provenance. Capturing again during retry changes
the accepted input. Store-owned interpretation would put conversation semantics
in mechanical storage; Context-owned CAS reads would break pure replay. Arbitrary
digest reads would transfer unrelated stored data to a model. Whole-history scans
would violate the bounded read contract already established by prepared reads.

## Consequences

Replay, process restart and exact retries preserve captured bytes. Changes to the
source never change an existing reference. Tampered previews, Header bindings,
CAS lengths and digests are refused. Fork reads require the exact inherited
parent interval. Boundary tests establish exclusion and pre-materialization
limits. Both clients preserve drafts on capture failure, show origin and omission
state, and support preview/removal before send. Native and authenticated remote
reads exercise the same contract.

### Trade-offs

The schema bump refuses the entire old Store and requires an explicitly selected
new Store; old data is preserved. Retain-all CAS includes abandoned captures.
Bounded suffix selection can omit earlier context or produce no exportable text;
the UI must report this instead of presenting the result as complete history.
