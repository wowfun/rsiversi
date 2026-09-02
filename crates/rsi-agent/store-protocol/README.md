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
