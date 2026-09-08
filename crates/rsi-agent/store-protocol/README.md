# rsi-agent-store-protocol

## Domain mutations

Domain state is canonical only in `DomainStateCommitted` controls. One record
contains all distinct-domain replacements for a bounded request, its expected
revisions and Kernel-assigned source. A receipt identifies that control and the
request digest. Domain head/as-of and request indexes retain control positions,
never a second authoritative state payload. Store does not invoke domain codecs.

A mixed Turn mutation binds its exact contiguous same-append Fact span and a
digest of those Fact bodies. The request digest includes that body digest,
but excludes allocated sequences and timestamps. Store admission validates the
binding before either stream changes; cold validation checks the canonical
Fact interval again. A retry cannot change Facts while retaining the same
domain request identity. Baselines and external commands do not claim Turn Facts.

The initial nonempty baseline is control one of the atomic fresh Header and
first acceptance commit; an omitted baseline denotes an empty domain set.
Only baseline admission creates a domain. Later mutations require an existing
matching codec and exact predecessor revision. Same request identity with
different content conflicts. Querying a committed request recovers the exact
receipt after an unknown result. Selecting a historical state uses the paired
terminal control horizon, so later idle mutations cannot enter that fork.

## Correlated terminal commits

Every terminal Fact is committed with one `TurnBoundaryRecorded` control in the
same `AtomicAgentCommit`. Each Session append contains at most one terminal;
its marker is the final control, names that exact Turn and Fact sequence, and
defines the terminal's control horizon. A marker without that same-append
terminal, a duplicate, a later business control, or a Fact-only terminal append
is rejected before mutation. Recovery repairs separate terminal boundaries in
separate appends. This permits one exact Fact/control prefix pair for every
completed historical Turn without inferring a control horizon from timestamps.

The terminal index retains both sequences and rolling prefix digests, derived
only from canonical records. Fork boundary reads return that exact pair.
`none` inherits neither prefix; `all` and `N` use the selected terminal's control
horizon, so later idle commands cannot alter an already selected historical
cut. Index and offline validation reject missing, mismatched or fabricated
terminal correlations. The schema cutover is explicit; old database files are
preserved and never automatically rewritten.

`inspect_session` is one bounded read snapshot of immutable Header, durable
Fact/control cursors, pending-message metadata, current Turn and activation
phase, and complete bounded descendant activity. It decodes no message bodies
or historical Facts. The Memory adapter holds one state lock; SQLite uses one
read transaction. Clients cannot reconstruct this contract by combining
independent reads across concurrent commits.

Mailbox entries retain immutable delivery intent, original acceptance timestamp,
and an optional bound steering Turn separately from their mutable current
target. Pending next-Step completion and bound human-steer messages can be
promoted at terminal settlement. Human-steer ready keys retain their acceptance
timestamp/control sequence; Completion ready keys retain promotion order.
Both adapters validate this against the canonical control stream.

This crate owns the mechanical durable seam for Agent sessions. A Store accepts
one immutable header, contiguous compare-and-append Fact batches, bounded
reads, session enumeration for recovery, and immutable CAS objects.
Append and atomic-commit suffixes own `Arc<SessionFact>` handles. Transferring
or retrying a prepared suffix shares immutable payloads; adapters validate and
encode borrowed Facts. The Memory adapter retains those same allocations.
Read pages keep their separate bounded materialization contract.
Alongside
the canonical session sequence it transactionally maintains mechanical turn
membership and open/terminal indexes. Those indexes select durable bytes; they
do not apply effect transitions, classify recovery, or select a turn outcome.
The protocol-owned `store_fact_turn_role` classifier is the single authority
for acceptance, terminal, and in-turn event membership used by Store adapters.
One exact turn-boundary read validates and returns the indexed acceptance
sequence, optional typed terminal Fact, and read-time durable watermark without
materializing intervening event Facts. Each returned Fact must match the exact
session sequence and relational turn/kind selected by its index; typed JSON
validity alone is not an index proof. Kernel alone interprets the terminal outcome.
See [the Agent architecture](../docs/architecture.md).

`header` and recent-session listing validate bounded immutable metadata only;
they do not scan history or establish a historical validation proof.
`validate_session` explicitly checks the mechanical durable session invariants.
Execution, recovery, lineage, and control decisions require that proof before
using a cold Header. Fact, control, turn, outcome, and mutation boundaries retain
their validation requirements. A backend may cache successful proofs within
its exclusive writer lifetime; metadata reads neither fill nor touch that cache.

Agent control commits compare both Fact and control watermarks and atomically
touch at most three sessions. The Store maintains mechanical indexes with
bounded query and atomic-update surfaces for ready messages, immutable
parent-child lineage, terminal-prefix digests,
a byte-bounded prefix plus the exact pending-mailbox count and direct message
status, and the one currently active activation per session. A lightweight
mailbox summary returns the pending count, the bounded ordered identities of
pending promotable next-Step messages, and Fact/control tails for capacity and
terminal-promotion decisions. The full mailbox read is one Store snapshot:
it never materializes the valid 64-message worst case at once, returns the Fact
and control tails observed by that snapshot, and lets callers reserve its fixed
page bound before I/O. Message admission
and status reads use that index rather than replaying an unbounded control
history. Each returned entry carries the exact encoded message length computed
by the Store while it reads or indexes the payload; mailbox validation and
Kernel next-Step batching reuse that value instead of serializing the same
validated message again. A descendant
status snapshot reads one subtree's immutable membership, parent/path/task,
control watermarks, and open-Turn, active-activation and waking-message flags
in one Store snapshot; Agent waits use it as their race-free
observation baseline. Activation guards check exact ownership before applying
appends; quiescence guards check the resulting indexed state before committing.
The Kernel chooses policy and the Store proves these atomic conditions. Guard failures have
dedicated errors and are not encoded as synthetic Fact-sequence conflicts. This
prevents a parent settlement from racing a child claim.
Quiescence guards name a subtree root, not a previously enumerated list. Within
the same write transaction, after all appends, the Store checks every strict
descendant against all three busy indexes. This includes children introduced by
that transaction. The root may itself be newly created and is excluded.
Traversal rejects cycles and trees exceeding 256 nodes.
An atomic append without a Header requires an existing Session. Its absence is
reported as `NotFound` before Fact or control cursor conflicts, identically in
the production backend and the shared in-memory contract fixture.

The lineage index is derived from immutable Headers rather than trusting
duplicated root labels in writes. A child root must equal its parent's derived
root, and the Kernel derives every accepted mailbox message root from the
target's prepared Header before it can enter either mailbox or ready indexes.
The Store revalidates that derived root. Memory, SQLite, and offline verification
enforce the same rule.

Forward and backward Session Fact reads, per-turn Fact reads, open-turn
enumeration, lexical session enumeration, lexical enumeration restricted to
sessions with open turns, and creation-time-ordered recent-session enumeration
are cursor-paginated with protocol-owned count and byte limits. Backward reads
take an exclusive sequence cursor and still return Facts in ascending sequence
order; a nonempty page ends at `before_seq - 1`, while only cursor one may
produce an empty page. The recent-session cursor is the exact descending
`(created_at_ms, session_id)` key; lexical enumeration remains the recovery
contract and is not reused for presentation. Startup recovery uses the
open-session index, so closed historical sessions do not impose Store calls or
decoding work. No Store method materializes an unbounded durable catalog.
Each recent-session row carries its validated bounded Header from the same Store
snapshot, so presentation does not fan one index page into hundreds of later
Header reads.

Context checkpoints are an optional cache, not canonical session state. The
Store preserves at most 64 MiB of opaque Context-owned bytes with a header
fingerprint, cursor, and lowercase SHA-256 digest of the folded Fact prefix, and
installs them only when that cursor exactly equals the durable tail and both
fingerprint and prefix digest equal Store-owned values derived from the
canonical header and Fact log. A missing, stale, corrupt, or unsupported
checkpoint never changes Fact replay semantics.

The Store also derives the latest instruction-baseline and skill-catalog
SHA-256 values from canonical input Facts. This bounded projection lets a cold
Kernel restore replacement suppression without replaying a whole closed
history; it is mechanical cache state, and offline verification recomputes it
from the Fact stream.

Activation starts must match the immutable Header's root, parent, and Agent
path. Store adapters enforce this relationship before committing any control
or index row; SQLite verification independently rechecks historical starts.

Inspection's pending-message metadata carries the immutable promotion capability
(Completion source or steering intent bound to a Turn), so a promoted Completion remains a valid
next-Turn route without materializing its payload.
