# rsi-agent-store-protocol

This crate owns the mechanical durable seam for Agent sessions. A Store accepts
one immutable header, contiguous compare-and-append Fact batches, bounded
reads, session enumeration for recovery, and immutable CAS objects. Alongside
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

Agent control commits compare both Fact and control watermarks and atomically
touch at most three sessions. The Store maintains mechanical indexes with
bounded query and atomic-update surfaces for ready messages, immutable
parent-child lineage, terminal-prefix digests,
a byte-bounded prefix plus the exact pending-mailbox count and direct message
status, and the one currently active activation per session. A lightweight
mailbox summary returns the pending count, the bounded ordered identities of
pending next-Step completion messages, and Fact/control tails for capacity and
terminal-promotion decisions. The full mailbox read is one Store snapshot:
it never materializes the valid 64-message worst case at once, returns the Fact
and control tails observed by that snapshot, and lets callers reserve its fixed
page bound before I/O. Message admission
and status reads use that index rather than replaying an unbounded control
history. Each returned entry carries the exact encoded message length computed
by the Store while it reads or indexes the payload; mailbox validation and
Kernel next-Step batching reuse that value instead of serializing the same
validated message again. A descendant
control snapshot reads one subtree's immutable membership and every member's
control watermark in one Store snapshot; Agent waits use it as their race-free
observation baseline. Activation and quiescence guards are compare conditions:
the Kernel chooses policy, while the Store only proves that the indexed durable
state still equals the state on which that choice was based. Guard failures have
dedicated errors and are not encoded as synthetic Fact-sequence conflicts. This
prevents a parent settlement from racing a child claim.
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
